#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C
unset TAR_OPTIONS

fail() { printf 'Proxywatch: %s\n' "$*" >&2; exit 1; }

usage() {
    printf '%s\n' \
        'Usage: bash install.sh [--version vX.Y.Z] [--from DIRECTORY]' \
        'Without options, install the latest public stable release for this Linux host.' \
        'For an offline install, provide both the version and the release assets directory.'
}

download() {
    curl -q --proto '=https' --proto-redir '=https' --tlsv1.2 \
        --fail --silent --show-error --location --retry 2 --connect-timeout 15 \
        --max-time 300 --output "$2" "$1"
}

regular_member() {
    local listing
    listing=$(tar --list --verbose --gzip --file="$work/archive" --quoting-style=escape -- "$1") ||
        fail "Missing archive member: $1"
    [[ $listing == -* && $listing != *$'\n'* ]] || fail "Expected one regular file: $1"
    tar --extract --to-stdout --gzip --file="$work/archive" -- "$1" > "$2"
    [[ -s $2 ]] || fail "Empty archive member: $1"
}

available_alias() {
    [[ ! -e $alias_path && ! -L $alias_path ]] || return 1
    ! command -v pwch >/dev/null 2>&1 || return 1
    local remaining="${PATH:-}:" directory
    while [[ -n $remaining ]]; do
        directory=${remaining%%:*}
        remaining=${remaining#*:}
        [[ ! -e ${directory:-.}/pwch && ! -L ${directory:-.}/pwch ]] || return 1
    done
}

claim_alias() {
    if available_alias; then
        if ln -sT -- proxywatch "$alias_path"; then
            printf 'You can also run pwch.\n'
        elif [[ ! -e $alias_path && ! -L $alias_path ]]; then
            fail "Could not create $alias_path"
        fi
    fi
}

version=''
source_dir=''
while (($#)); do
    case "$1" in
        --version|--from)
            (($# >= 2)) && [[ -n $2 && $2 != --* ]] || fail "Missing argument for $1"
            if [[ $1 == --version ]]; then version=$2; else source_dir=$2; fi
            shift 2 ;;
        --help|-h) usage; exit 0 ;;
        *) fail "Unknown option: $1 (see --help)" ;;
    esac
done

semver='(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)'
[[ -z $version || $version =~ ^v${semver}$ ]] || fail 'Expected a stable version like v0.1.0'
[[ -z $source_dir || -n $version && -d $source_dir ]] || fail '--from requires a directory and --version'
[[ $(uname -s) == Linux ]] || fail 'Only Linux is supported'
case "$(uname -m)" in
    x86_64|amd64) architecture=x86_64; machine=3e00 ;;
    aarch64|arm64) architecture=aarch64; machine=b700 ;;
    *) fail "Unsupported Linux architecture: $(uname -m)" ;;
esac
((EUID == 0)) || fail 'Run with sudo or as root to install in /usr/local'

binary=/usr/local/bin/proxywatch
alias_path=/usr/local/bin/pwch
unit=/usr/local/lib/systemd/user/proxywatch.service
for target in "$binary" "$unit"; do
    [[ ! -L $target && ( ! -e $target || -f $target ) ]] || fail "Refusing to replace $target"
done

umask 022
work=$(mktemp -d)
binary_stage=''
unit_stage=''
cleanup() {
    if [[ -n $binary_stage ]]; then rm -f -- "$binary_stage"; fi
    if [[ -n $unit_stage ]]; then rm -f -- "$unit_stage"; fi
    rm -f -- "$work/SHA256SUMS" "$work/archive" "$work/proxywatch" "$work/proxywatch.service"
    rmdir -- "$work"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

releases=https://github.com/nahashed/proxywatch/releases
if [[ -n $source_dir ]]; then
    cp -- "$source_dir/SHA256SUMS" "$work/SHA256SUMS"
elif [[ -n $version ]]; then
    download "$releases/download/$version/SHA256SUMS" "$work/SHA256SUMS"
else
    download "$releases/latest/download/SHA256SUMS" "$work/SHA256SUMS"
fi

pattern="^([[:xdigit:]]{64})[[:blank:]]+\*?(proxywatch-(${semver})-linux-${architecture}\.tar\.gz)$"
count=0
while IFS= read -r line || [[ -n $line ]]; do
    [[ $line =~ $pattern ]] || continue
    candidate=v${BASH_REMATCH[3]}
    [[ -z $version || $candidate == "$version" ]] || continue
    digest=${BASH_REMATCH[1]}
    asset=${BASH_REMATCH[2]}
    tag=$candidate
    ((count+=1))
done < "$work/SHA256SUMS"
((count == 1)) || fail "Expected one checksum for the selected linux-$architecture archive"

if [[ -n $source_dir ]]; then
    cp -- "$source_dir/$asset" "$work/archive"
else
    download "$releases/download/$tag/$asset" "$work/archive"
fi
actual=$(sha256sum < "$work/archive")
[[ ${actual%% *} == "${digest,,}" ]] || fail 'Archive SHA-256 mismatch, nothing installed'
archive_root=${asset%.tar.gz}
regular_member "$archive_root/proxywatch" "$work/proxywatch"
regular_member "$archive_root/deploy/proxywatch.service" "$work/proxywatch.service"
header=$(od -An -tx1 -N6 "$work/proxywatch" | tr -d '[:space:]')
elf_machine=$(od -An -tx1 -j18 -N2 "$work/proxywatch" | tr -d '[:space:]')
[[ $header == 7f454c460201 && $elf_machine == "$machine" ]] || fail "Expected an ELF64 binary for $architecture"

# Compare trusted release contents without executing a pre-existing binary as root.
exec {install_lock}</usr/local
flock -x "$install_lock"
if [[ -x $binary && -s $unit ]] && cmp --silent -- "$work/proxywatch" "$binary" &&
    cmp --silent -- "$work/proxywatch.service" "$unit"; then
    printf 'Proxywatch %s is already installed.\n' "$tag"
    claim_alias
    exit 0
fi

updating=false
if [[ -f $binary ]]; then updating=true; fi
mkdir -p -- "${binary%/*}" "${unit%/*}"
binary_stage=$(mktemp "${binary%/*}/.proxywatch.XXXXXXXX")
unit_stage=$(mktemp "${unit%/*}/.proxywatch-service.XXXXXXXX")
install -m 0755 -- "$work/proxywatch" "$binary_stage"
install -m 0644 -- "$work/proxywatch.service" "$unit_stage"
mv -fT -- "$unit_stage" "$unit"
mv -fT -- "$binary_stage" "$binary"

if [[ $updating == true ]]; then
    printf 'Updated Proxywatch on disk to %s. Running instances need a restart.\n' "$tag"
    printf 'For a running user service, run as its owner: systemctl --user daemon-reload && systemctl --user restart proxywatch.service\n'
else
    printf 'Installed Proxywatch %s. Run proxywatch to open setup.\n' "$tag"
fi
claim_alias
printf 'No services were started or restarted. Existing settings are unchanged.\n'
