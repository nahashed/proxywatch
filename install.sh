#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C
user_path=${PATH:-}
export PATH=/usr/sbin:/usr/bin:/sbin:/bin
unset TAR_OPTIONS UNZIP UNZIPOPT

fail() { printf 'Proxywatch: %s\n' "$*" >&2; exit 1; }

usage() {
    printf '%s\n' \
        'Usage: bash install.sh [--version vX.Y.Z] [--from DIRECTORY] [--with-xray] [--non-interactive]' \
        'Without options, install the latest public stable release for this Linux host.' \
        'For an offline install, provide both the version and the release assets directory.' \
        'Use --with-xray to install a private engine if no supported Xray is found.' \
        'Use --non-interactive to skip all prompts and the setup wizard.'
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
    ! PATH="$user_path" command -v pwch >/dev/null 2>&1 || return 1
    local remaining="$user_path:" directory
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

verify_elf() {
    local header elf_machine
    header=$(od -An -tx1 -N6 "$1" | tr -d '[:space:]')
    elf_machine=$(od -An -tx1 -j18 -N2 "$1" | tr -d '[:space:]')
    [[ $header == 7f454c460201 && $elf_machine == "$machine" ]] || fail "Expected an ELF64 binary for $architecture"
}

trusted_path() {
    local path=$1 owner mode
    while [[ $path != / ]]; do
        [[ ! -L $path ]] || return 1
        read -r owner mode < <(stat -c '%u %a' -- "$path") || return 1
        [[ $owner == 0 ]] && (( (8#$mode & 022) == 0 )) || return 1
        path=${path%/*}
        path=${path:-/}
    done
}

ensure_directory() {
    local directory=$1
    if [[ ! -e $directory && ! -L $directory ]]; then
        ensure_directory "${directory%/*}"
        mkdir -m 0755 -- "$directory"
    fi
    [[ -d $directory ]] && trusted_path "$directory" || fail "Expected a secure root-owned directory: $directory"
}

check_xray() {
    local path=$1 version_output detected_version
    [[ -f $path && -x $path ]] && trusted_path "$path" || return 1
    # Even a locally installed engine only needs unprivileged access to report its version.
    version_output=$(cd / && timeout -k 1 4 setpriv --reuid=65534 --regid=65534 \
        --clear-groups --no-new-privs env -i PATH=/usr/bin:/bin HOME=/nonexistent \
        "$path" version 2>/dev/null) || return 1
    [[ $version_output =~ ^Xray[[:space:]]+([0-9]+\.[0-9]+\.[0-9]+)([[:space:]]|$) ]] || return 1
    detected_version=${BASH_REMATCH[1]}
    if [[ $(printf '%s\n' "$detected_version" "$xray_version" | sort -V | head -n 1) != "$xray_version" ]]; then
        printf 'Xray %s at %s is older than %s.\n' "$detected_version" "$path" "$xray_version"
        return 1
    fi
    selected_xray=$path
    printf 'Using Xray %s at %s.\n' "$detected_version" "$path"
}

prepare_xray() {
    local path answer archive expected actual directory
    for path in "$private_xray" /usr/local/bin/xray /usr/bin/xray; do
        if check_xray "$path"; then return 0; fi
    done
    printf 'Proxywatch needs Xray %s or later.\n' "$xray_version"
    if [[ $with_xray != true ]]; then
        if [[ $interactive != true ]]; then
            printf 'Use --with-xray to install the tested engine for Proxywatch.\n'
            return 0
        fi
        printf 'Install Xray %s for Proxywatch at %s? [y/N] ' "$xray_version" "$private_xray"
        if ! read -r answer || [[ ${answer,,} != y && ${answer,,} != yes ]]; then
            printf 'Set the Xray path in Proxywatch before starting the daemon.\n'
            return 0
        fi
    fi
    case "$architecture" in
        x86_64) archive=Xray-linux-64.zip; expected=8195d909f1109b8f3d99eefe401a3c451d7bf4af71f24d3815420f77e5dd2a40 ;;
        aarch64) archive=Xray-linux-arm64-v8a.zip; expected=f5698bb218ada3b4022db26fafc39601c5f53b46b19eb76c9616325985807501 ;;
    esac
    if [[ -n $source_dir ]]; then
        [[ -f $source_dir/$archive ]] || fail "Place the official v$xray_version $archive in the offline release directory"
        cp -- "$source_dir/$archive" "$work/xray.zip"
    else
        download "https://github.com/XTLS/Xray-core/releases/download/v$xray_version/$archive" "$work/xray.zip"
    fi
    actual=$(sha256sum < "$work/xray.zip")
    [[ ${actual%% *} == "$expected" ]] || fail 'Xray archive SHA-256 mismatch'
    if command -v unzip >/dev/null; then
        unzip -p "$work/xray.zip" xray > "$work/xray"
    elif command -v python3 >/dev/null; then
        python3 -I -c 'import shutil, sys, zipfile
with zipfile.ZipFile(sys.argv[1]) as archive, archive.open("xray") as binary:
    shutil.copyfileobj(binary, sys.stdout.buffer)' "$work/xray.zip" > "$work/xray"
    else
        fail 'Install unzip or Python 3 to extract the verified Xray archive, then rerun the installer'
    fi
    verify_elf "$work/xray"
    directory=${private_xray%/*}
    ensure_directory "$directory"
    [[ ! -L $private_xray && ( ! -e $private_xray || -f $private_xray ) ]] || fail 'Unsafe private Xray target'
    xray_stage=$(mktemp "$directory/.xray.XXXXXXXX")
    install -m 0755 -- "$work/xray" "$xray_stage"
    mv -fT -- "$xray_stage" "$private_xray"
    selected_xray=$private_xray
    xray_changed=true
    printf 'Installed Xray %s at %s.\n' "$xray_version" "$private_xray"
}

as_owner() {
    if [[ -n ${SUDO_UID:-} && ${SUDO_UID:-} != 0 ]]; then
        [[ $SUDO_UID =~ ^[0-9]+$ ]] || fail 'Invalid sudo user ID'
        local account
        account=$(id -nu -- "$SUDO_UID") || fail 'Cannot resolve the invoking account'
        runuser -u "$account" -- env -u XDG_CONFIG_HOME \
            XDG_RUNTIME_DIR="/run/user/$SUDO_UID" DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$SUDO_UID/bus" \
            PATH=/usr/local/bin:/usr/bin:/bin "$@"
    else
        env PATH=/usr/local/bin:/usr/bin:/bin "$@"
    fi
}

apply_update() {
    local fragment dropins
    if [[ -x /usr/bin/systemctl ]] && as_owner /usr/bin/systemctl --user is-active --quiet proxywatch.service; then
        fragment=$(as_owner /usr/bin/systemctl --user show --property=FragmentPath --value proxywatch.service) || fail 'Could not identify the active user service'
        dropins=$(as_owner /usr/bin/systemctl --user show --property=DropInPaths --value proxywatch.service) || fail 'Could not identify user service overrides'
        if [[ $fragment == "$unit" && -z $dropins ]]; then
            as_owner /usr/bin/systemctl --user daemon-reload
            as_owner /usr/bin/systemctl --user try-restart proxywatch.service
            as_owner /usr/bin/systemctl --user is-active --quiet proxywatch.service || fail 'Proxywatch user service did not become active'
            printf 'Restarted the active Proxywatch user service.\n'
            return 0
        fi
    fi
    printf 'Running instances need a restart. For a user service, run as its owner: systemctl --user daemon-reload && systemctl --user restart proxywatch.service\n'
}

version=''
source_dir=''
with_xray=false
interactive=false
if [[ -t 0 && -t 1 ]]; then interactive=true; fi
while (($#)); do
    case "$1" in
        --version|--from)
            (($# >= 2)) && [[ -n $2 && $2 != --* ]] || fail "Missing argument for $1"
            if [[ $1 == --version ]]; then version=$2; else source_dir=$2; fi
            shift 2 ;;
        --with-xray) with_xray=true; shift ;;
        --non-interactive) interactive=false; shift ;;
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

xray_version=26.7.28
private_xray=/usr/local/lib/proxywatch/xray
selected_xray=''
xray_changed=false
binary=/usr/local/bin/proxywatch
alias_path=/usr/local/bin/pwch
unit=/usr/local/lib/systemd/user/proxywatch.service
for target in "$binary" "$unit"; do
    [[ ! -L $target && ( ! -e $target || -f $target ) ]] || fail "Refusing to replace $target"
done

umask 022
work=$(mktemp -d /tmp/proxywatch-install.XXXXXXXX)
binary_stage=''
unit_stage=''
xray_stage=''
cleanup() {
    if [[ -n $binary_stage ]]; then rm -f -- "$binary_stage"; fi
    if [[ -n $unit_stage ]]; then rm -f -- "$unit_stage"; fi
    if [[ -n $xray_stage ]]; then rm -f -- "$xray_stage"; fi
    rm -f -- "$work/xray.zip" "$work/xray" "$work/SHA256SUMS" "$work/archive" "$work/proxywatch" "$work/proxywatch.service"
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
verify_elf "$work/proxywatch"

# Compare trusted release contents without executing a pre-existing binary as root.
exec {install_lock}</usr/local
flock -x "$install_lock"
ensure_directory "${binary%/*}"
ensure_directory "${unit%/*}"
prepare_xray
if [[ -x $binary && -s $unit ]] && cmp --silent -- "$work/proxywatch" "$binary" &&
    cmp --silent -- "$work/proxywatch.service" "$unit"; then
    printf 'Proxywatch %s is already installed.\n' "$tag"
    claim_alias
    flock -u "$install_lock"
    if [[ $xray_changed == true ]]; then apply_update; fi
    exit 0
fi

updating=false
if [[ -f $binary ]]; then updating=true; fi
binary_stage=$(mktemp "${binary%/*}/.proxywatch.XXXXXXXX")
unit_stage=$(mktemp "${unit%/*}/.proxywatch-service.XXXXXXXX")
install -m 0755 -- "$work/proxywatch" "$binary_stage"
install -m 0644 -- "$work/proxywatch.service" "$unit_stage"
mv -fT -- "$unit_stage" "$unit"
mv -fT -- "$binary_stage" "$binary"

claim_alias
flock -u "$install_lock"
if [[ $updating == true ]]; then
    printf 'Updated Proxywatch on disk to %s.\n' "$tag"
    apply_update
else
    printf 'Installed Proxywatch %s. Run proxywatch to open setup.\n' "$tag"
    if [[ $interactive == true ]]; then
        as_owner "PROXYWATCH_SETUP_XRAY=$selected_xray" "$binary" tui
    fi
fi
