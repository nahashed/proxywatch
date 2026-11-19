#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C
user_path=${PATH:-}
export PATH=/usr/sbin:/usr/bin:/sbin:/bin
unset TAR_OPTIONS UNZIP UNZIPOPT

fail() { printf 'Proxywatch: %s\n' "$*" >&2; exit 1; }

usage() {
    printf '%s\n' \
        'Usage: bash install.sh [--version vX.Y.Z] [--from DIRECTORY] [--with-xray|--without-xray] [--non-interactive]' \
        'Without options, install the latest public stable release for this Linux host.' \
        'For an offline install, provide both the version and the release assets directory.' \
        'Xray is installed for Proxywatch when no supported engine is found (--with-xray).' \
        'Use --without-xray to manage the engine yourself.' \
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
    local path archive expected actual directory
    for path in "$private_xray" /usr/local/bin/xray /usr/bin/xray; do
        if check_xray "$path"; then return 0; fi
    done
    printf 'Proxywatch needs Xray %s or later.\n' "$xray_version"
    if [[ $with_xray != true ]]; then
        printf 'Xray installation skipped (--without-xray). Set the engine path before starting Proxywatch.\n'
        return 0
    fi
    case "$architecture" in
        x86_64) archive=Xray-linux-64.zip; expected=8195d909f1109b8f3d99eefe401a3c451d7bf4af71f24d3815420f77e5dd2a40 ;;
        aarch64) archive=Xray-linux-arm64-v8a.zip; expected=f5698bb218ada3b4022db26fafc39601c5f53b46b19eb76c9616325985807501 ;;
    esac
    if [[ -n $source_dir ]]; then
        [[ -f $source_dir/$archive ]] || fail "Place the official v$xray_version $archive in the offline release directory, or use --without-xray to manage Xray yourself"
        cp -- "$source_dir/$archive" "$work/xray.zip"
    else
        download "https://github.com/XTLS/Xray-core/releases/download/v$xray_version/$archive" "$work/xray.zip"
    fi
    actual=$(sha256sum < "$work/xray.zip")
    [[ ${actual%% *} == "$expected" ]] || fail 'Xray archive SHA-256 mismatch'
    if ! command -v unzip >/dev/null && ! command -v python3 >/dev/null; then
        [[ -z $source_dir ]] || fail 'Offline Xray extraction requires unzip or Python 3'
        command -v apt-get >/dev/null || fail 'Install unzip or Python 3 to extract the verified Xray archive'
        printf 'Installing unzip with APT to extract the verified Xray archive.\n'
        if ! apt-cache policy unzip | awk '$1 == "Candidate:" && $2 != "(none)" { found=1 } END { exit !found }'; then
            DEBIAN_FRONTEND=noninteractive apt-get update
        fi
        DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends --no-upgrade unzip
    fi
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

prepare_owner() {
    local uid=${SUDO_UID:-0} runtime socket account
    local -a bus_environment
    [[ $uid =~ ^[0-9]+$ ]] || fail 'Invalid sudo user ID'
    runtime=/run/user/$uid
    owner_command=(env -u XDG_RUNTIME_DIR -u DBUS_SESSION_BUS_ADDRESS)
    if [[ $uid != 0 ]]; then
        account=$(id -nu -- "$uid") || fail 'Cannot resolve the invoking account'
        owner_command=(runuser -u "$account" -- "${owner_command[@]}" -u XDG_CONFIG_HOME)
    fi
    owner_command+=(PATH=/usr/local/bin:/usr/bin:/bin)
    manager_available=false
    # Container consoles can lack login-session variables even with a live user manager.
    if [[ -d $runtime && ! -L $runtime && $(stat -c '%u:%a' -- "$runtime") == "$uid:700" ]] && trusted_path /run/user; then
        for socket in "$runtime/bus" "$runtime/systemd/private"; do
            [[ -S $socket && ! -L $socket && $(stat -c %u -- "$socket") == "$uid" ]] || continue
            bus_environment=(XDG_RUNTIME_DIR="$runtime")
            if [[ $socket == "$runtime/bus" ]]; then
                bus_environment+=(DBUS_SESSION_BUS_ADDRESS="unix:path=$socket")
            fi
            if [[ -x /usr/bin/systemctl ]] &&
                "${owner_command[@]}" "${bus_environment[@]}" /usr/bin/systemctl --user show-environment >/dev/null 2>&1; then
                owner_command+=("${bus_environment[@]}")
                manager_available=true
                break
            fi
        done
    fi
}

owner_systemctl() {
    "${owner_command[@]}" /usr/bin/systemctl --user "$@" 2>/dev/null
}

install_system_unit() {
    [[ -f $work/system.service ]] || return 0
    ensure_directory "${system_unit%/*}"
    ensure_directory "${unit_receipt%/*}"
    if [[ -e $unit_receipt || -L $unit_receipt ]]; then
        if [[ ! -f $unit_receipt ]] || ! trusted_path "$unit_receipt"; then
            printf 'Skipping the system service: its installation receipt is not a trusted file.\n'
            return 0
        fi
    fi
    local current='' digest recorded=''
    digest=$(sha256sum < "$work/system.service")
    digest=${digest%% *}
    if [[ -e $system_unit || -L $system_unit ]]; then
        if [[ -f $system_unit ]] && trusted_path "$system_unit"; then
            current=$(sha256sum < "$system_unit")
            current=${current%% *}
        fi
        if [[ -f $unit_receipt ]]; then recorded=$(<"$unit_receipt"); fi
        if [[ -z $current || $current != "$digest" && $current != "$recorded" ]]; then
            printf 'Preserved the customized system service at %s. Review and restart it manually if needed.\n' "$system_unit"
            return 0
        fi
    elif [[ ! -e $unit_receipt ]]; then
        system_unit_new=true
    fi
    if [[ $current != "$digest" ]]; then
        system_stage=$(mktemp "${system_unit%/*}/.proxywatch-service.XXXXXXXX")
        install -m 0644 -- "$work/system.service" "$system_stage"
        mv -fT -- "$system_stage" "$system_unit"
        system_unit_changed=true
        printf 'Installed the Proxywatch system service definition.\n'
    fi
    if [[ $recorded != "$digest" ]]; then
        system_stage=$(mktemp "${unit_receipt%/*}/.system-unit.XXXXXXXX")
        printf '%s\n' "$digest" > "$system_stage"
        mv -fT -- "$system_stage" "$unit_receipt"
    fi
    system_unit_managed=true
}

system_control() {
    env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin HOME=/root /usr/bin/systemctl --system --no-ask-password "$@" 2>/dev/null
}

distributed_system_service() {
    local fragment names dropins
    fragment=$(system_control show --property=FragmentPath --value proxywatch.service) || return 1
    names=$(system_control show --property=Names --value proxywatch.service) || return 1
    dropins=$(system_control show --property=DropInPaths --value proxywatch.service) || return 1
    [[ $fragment == "$system_unit" && $names == proxywatch.service && -z $dropins ]] || return 1
    cmp -s "$work/system.service" "$system_unit" && trusted_path "$system_unit"
}

apply_system_service() {
    local changed=$1 enabled
    [[ ${SUDO_UID:-0} == 0 &&
        ${XDG_CONFIG_HOME:-${HOME:-/root}/.config} == /root/.config &&
        -d /run/systemd/system && -x /usr/bin/systemctl ]] || return 1
    local settings=/root/.config/proxywatch/settings.json
    [[ -f $settings ]] && trusted_path "$settings" || return 1
    if [[ $system_unit_managed != true ]]; then
        if [[ $changed == true && ! -e $work/system.service ]] && system_control is-active --quiet proxywatch.service; then
            printf 'This release has no system service definition. The active system service needs a manual restart to use the installed files.\n'
            return 0
        fi
        return 1
    fi
    if system_control is-active --quiet proxywatch.service; then
        if [[ $changed == true || $system_unit_changed == true ]]; then
            if ! distributed_system_service; then
                printf 'The active system service is customized. Review and restart it manually.\n'
                return 0
            fi
            system_control daemon-reload || fail 'Could not reload the system service manager'
            distributed_system_service || fail 'The system service changed during the update. Check it before restarting.'
            system_control try-restart proxywatch.service && system_control is-active --quiet proxywatch.service ||
                fail 'Proxywatch system service did not restart'
            printf 'Restarted the active Proxywatch system service.\n'
        fi
        return 0
    fi
    [[ $manager_available == false ]] || return 1
    enabled=$(system_control is-enabled proxywatch.service) || enabled=''
    if [[ $system_unit_new == true || $enabled == enabled ]]; then
        trusted_path "$binary" || fail 'The installed Proxywatch executable is not trusted'
        env -i HOME=/root XDG_CONFIG_HOME=/root/.config PATH=/usr/local/bin:/usr/bin:/bin \
            "$binary" activate-system-service </dev/null || fail 'Could not activate the Proxywatch system service'
        return 0
    fi
    return 1
}

apply_update() {
    local fragment dropins
    if [[ $manager_available != true ]]; then
        printf 'User service manager unavailable. Restart any running foreground instance to use the updated files.\n'
        return
    fi
    if ! owner_systemctl is-active --quiet proxywatch.service; then
        printf 'No active Proxywatch user service. Restart any running foreground instance to use the updated files.\n'
        return
    fi
    fragment=$(owner_systemctl show --property=FragmentPath --value proxywatch.service) || fail 'Could not identify the active user service'
    dropins=$(owner_systemctl show --property=DropInPaths --value proxywatch.service) || fail 'Could not identify user service overrides'
    if [[ $fragment != "$unit" || -n $dropins ]]; then
        printf 'The active Proxywatch user service is customized. Restart it as its owner to use the updated files.\n'
        return
    fi
    owner_systemctl daemon-reload &&
        owner_systemctl try-restart proxywatch.service &&
        owner_systemctl is-active --quiet proxywatch.service ||
        fail 'Proxywatch user service did not restart'
    printf 'Restarted the active Proxywatch user service.\n'
}

finish_install() {
    local changed=$1
    claim_alias
    prepare_owner
    # Newly installed fragments may have been cached as missing by an existing manager.
    if [[ $user_unit_changed == true && $manager_available == true ]] && ! owner_systemctl is-active --quiet proxywatch.service; then
        owner_systemctl daemon-reload || printf 'Could not reload the user service manager.\n'
    fi
    if [[ $system_unit_changed == true && ${SUDO_UID:-0} == 0 && $manager_available == false &&
        -d /run/systemd/system && -x /usr/bin/systemctl ]] && ! system_control is-active --quiet proxywatch.service; then
        system_control daemon-reload || printf 'Could not reload the system service manager.\n'
    fi
    if ! apply_system_service "$changed" && [[ $changed == true ]]; then apply_update; fi
    flock -u "$install_lock"
    exec {install_lock}<&-
    if "${owner_command[@]}" /bin/sh -c 'test -f "${XDG_CONFIG_HOME-$HOME/.config}/proxywatch/settings.json"'; then return; fi
    if [[ $interactive != true ]]; then
        printf 'Setup is pending. Run proxywatch as the service owner to configure it.\n'
        return
    fi
    printf 'Opening first-time setup.\n'
    cleanup
    trap - EXIT HUP INT TERM
    exec "${owner_command[@]}" PROXYWATCH_INSTALL_SETUP=1 "PROXYWATCH_SETUP_XRAY=$selected_xray" "$binary" tui
}

version=''
source_dir=''
with_xray=true
interactive=false
if [[ -t 0 && -t 1 ]]; then interactive=true; fi
while (($#)); do
    case "$1" in
        --version|--from)
            (($# >= 2)) && [[ -n $2 && $2 != --* ]] || fail "Missing argument for $1"
            if [[ $1 == --version ]]; then version=$2; else source_dir=$2; fi
            shift 2 ;;
        --with-xray) with_xray=true; shift ;;
        --without-xray) with_xray=false; shift ;;
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
system_unit=/usr/local/lib/systemd/system/proxywatch.service
unit_receipt=/usr/local/lib/proxywatch/system-unit.sha256
system_unit_new=false
system_unit_changed=false
system_unit_managed=false
user_unit_changed=false
for target in "$binary" "$unit"; do
    [[ ! -L $target && ( ! -e $target || -f $target ) ]] || fail "Refusing to replace $target"
done

umask 022
work=$(mktemp -d /tmp/proxywatch-install.XXXXXXXX)
binary_stage=''
unit_stage=''
system_stage=''
xray_stage=''
cleanup() {
    if [[ -n $binary_stage ]]; then rm -f -- "$binary_stage"; fi
    if [[ -n $unit_stage ]]; then rm -f -- "$unit_stage"; fi
    if [[ -n $system_stage ]]; then rm -f -- "$system_stage"; fi
    if [[ -n $xray_stage ]]; then rm -f -- "$xray_stage"; fi
    rm -f -- "$work/xray.zip" "$work/xray" "$work/SHA256SUMS" "$work/archive" "$work/proxywatch" "$work/proxywatch.service" "$work/system.service"
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
system_member="$archive_root/deploy/system/proxywatch.service"
if tar --list --gzip --file="$work/archive" -- "$system_member" >/dev/null 2>&1; then
    regular_member "$system_member" "$work/system.service"
elif [[ $(printf '%s\n' "$tag" v0.1.2 | sort -V | head -n 1) == v0.1.2 ]]; then
    fail "Missing archive member: $system_member"
fi
verify_elf "$work/proxywatch"

# Compare trusted release contents without executing a pre-existing binary as root.
exec {install_lock}</usr/local
flock -x "$install_lock"
ensure_directory "${binary%/*}"
ensure_directory "${unit%/*}"
prepare_xray
install_system_unit
if [[ -x $binary && -s $unit ]] && trusted_path "$binary" && cmp --silent -- "$work/proxywatch" "$binary" &&
    cmp --silent -- "$work/proxywatch.service" "$unit"; then
    printf 'Proxywatch %s is already installed.\n' "$tag"
    finish_install "$xray_changed"
    exit 0
fi

updating=false
if [[ -f $binary ]]; then updating=true; fi
binary_stage=$(mktemp "${binary%/*}/.proxywatch.XXXXXXXX")
install -m 0755 -- "$work/proxywatch" "$binary_stage"
if ! cmp --silent -- "$work/proxywatch.service" "$unit" 2>/dev/null; then
    unit_stage=$(mktemp "${unit%/*}/.proxywatch-service.XXXXXXXX")
    install -m 0644 -- "$work/proxywatch.service" "$unit_stage"
    mv -fT -- "$unit_stage" "$unit"
    user_unit_changed=true
fi
mv -fT -- "$binary_stage" "$binary"

if [[ $updating == true ]]; then
    printf 'Updated Proxywatch on disk to %s.\n' "$tag"
else
    printf 'Installed Proxywatch %s.\n' "$tag"
fi
finish_install "$updating"
