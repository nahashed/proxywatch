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

name_is_free() {
    local name=$1 remaining="$user_path:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:" directory path
    # Also inspect dangling links, which command -v does not report. Treat only our
    # own links and the installed binary as available when repairing another link.
    case $(type -t -- "$name" || true) in function|alias|builtin|keyword) return 1 ;; esac
    while [[ -n $remaining ]]; do
        directory=${remaining%%:*}
        remaining=${remaining#*:}
        path=${directory:-.}/$name
        if [[ -e $path || -L $path ]]; then
            [[ $path -ef $binary ]] || return 1
        fi
    done
}

link_command() {
    local target=$1 link=$2
    [[ ! -L $link && ! -e $link ]] || return 0
    # Never replace a concurrent installer's file or change a read-only system directory.
    if ! ln -sT -- "$target" "$link" 2>/dev/null && [[ ! -e $link && ! -L $link ]]; then
        printf 'Could not create %s. Run %s to open Proxywatch.\n' "$link" "$binary"
    fi
}

install_commands() {
    local system_bin=/usr/bin
    if name_is_free proxywatch && trusted_path "$system_bin"; then
        link_command "$binary" "$system_bin/proxywatch"
    fi
    if name_is_free pwch; then
        link_command proxywatch "$alias_path"
        if [[ $alias_path -ef $binary ]] && trusted_path "$system_bin"; then
            link_command "$binary" "$system_bin/pwch"
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

install_unit() {
    local source=$1 target=$2 receipt=$3 previous=$4 current='' recorded='' digest
    installed_unit_new=false
    installed_unit_changed=false
    installed_unit_managed=false
    [[ -f $source ]] || return 0
    ensure_directory "${target%/*}"
    ensure_directory "${receipt%/*}"
    if [[ -e $receipt || -L $receipt ]]; then
        if [[ ! -f $receipt ]] || ! trusted_path "$receipt"; then
            printf 'Skipping %s: its installation receipt is not a trusted file.\n' "$target"
            return 0
        fi
        recorded=$(<"$receipt")
        previous=''
    fi
    digest=$(sha256sum < "$source")
    digest=${digest%% *}
    if [[ -e $target || -L $target ]]; then
        if [[ -f $target ]] && trusted_path "$target"; then
            current=$(sha256sum < "$target")
            current=${current%% *}
        fi
        if [[ -z $current || $current != "$digest" && $current != "$recorded" && $current != "$previous" ]]; then
            printf 'Preserved the customized service at %s. Review and restart it manually if needed.\n' "$target"
            return 0
        fi
    elif [[ ! -e $receipt ]]; then
        installed_unit_new=true
    fi
    if [[ $current != "$digest" ]]; then
        unit_stage=$(mktemp "${target%/*}/.proxywatch-service.XXXXXXXX")
        install -m 0644 -- "$source" "$unit_stage"
        mv -fT -- "$unit_stage" "$target"
        installed_unit_changed=true
        printf 'Installed the Proxywatch service definition at %s.\n' "$target"
    fi
    if [[ $recorded != "$digest" ]]; then
        unit_stage=$(mktemp "${receipt%/*}/.unit-digest.XXXXXXXX")
        printf '%s\n' "$digest" > "$unit_stage"
        mv -fT -- "$unit_stage" "$receipt"
    fi
    installed_unit_managed=true
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

distributed_user_service() {
    local fragment dropins
    [[ $user_unit_managed == true ]] || return 1
    fragment=$(owner_systemctl show --property=FragmentPath --value proxywatch.service) || return 1
    dropins=$(owner_systemctl show --property=DropInPaths --value proxywatch.service) || return 1
    [[ $fragment == "$unit" && -z $dropins ]] && cmp -s "$work/proxywatch.service" "$unit" && trusted_path "$unit"
}

needs_recovery() {
    local state
    state=$("$1" show --property=ActiveState,SubState proxywatch.service) || return 1
    case $'\n'$state$'\n' in
        *$'\nActiveState=failed\n'*|*$'\nSubState=auto-restart\n'*|*$'\nSubState=auto-restart-queued\n'*) return 0 ;;
    esac
    return 1
}

open_recovery_menu() {
    local control=$1
    if [[ $("$control" show --property=NeedDaemonReload --value proxywatch.service) == yes ]]; then
        "$control" daemon-reload || fail 'Could not reload the updated Proxywatch service definition'
    fi
    recovery=true
}

restart_packaged_service() {
    local control=$1 check=$2 scope=$3
    "$check" || { printf 'The active Proxywatch %s service is customized. Restart it manually if needed.\n' "$scope"; return 0; }
    "$control" daemon-reload || fail "Could not reload the $scope service manager"
    "$check" || fail "The $scope service changed during the update. Check it before restarting."
    if needs_recovery "$control"; then
        recovery=true
    elif "$control" try-restart proxywatch.service && "$control" is-active --quiet proxywatch.service; then
        printf 'Restarted the active Proxywatch %s service.\n' "$scope"
    elif needs_recovery "$control"; then
        recovery=true
    else
        fail "Proxywatch $scope service did not restart"
    fi
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
    if needs_recovery system_control && distributed_system_service; then
        if [[ $manager_available == true && $(system_control show --property=ActiveState --value proxywatch.service) == failed ]]; then
            # A disabled, failed system unit no longer owns the installation. The root
            # user manager may already be running the replacement on the same ports.
            enabled=$(system_control is-enabled proxywatch.service) || :
            [[ $enabled != disabled ]] || return 1
        fi
        open_recovery_menu system_control
        return 0
    fi
    if system_control is-active --quiet proxywatch.service; then
        if [[ $changed == true || $system_unit_changed == true ]]; then
            restart_packaged_service system_control distributed_system_service system
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
    local changed=$1
    if [[ $manager_available == true ]] && needs_recovery owner_systemctl && distributed_user_service; then
        open_recovery_menu owner_systemctl
        return 0
    fi
    [[ $changed == true || $user_unit_changed == true ]] || return 0
    if [[ $manager_available != true ]]; then
        printf 'User service manager unavailable. Restart any running foreground instance to use the updated files.\n'
        return
    fi
    if ! owner_systemctl is-active --quiet proxywatch.service; then
        printf 'No active Proxywatch user service. Restart any running foreground instance to use the updated files.\n'
        return
    fi
    restart_packaged_service owner_systemctl distributed_user_service user
}

finish_install() {
    local changed=$1 recovery=false
    install_commands
    prepare_owner
    # Newly installed fragments may have been cached as missing by an existing manager.
    if [[ $user_unit_changed == true && $manager_available == true ]] && ! owner_systemctl is-active --quiet proxywatch.service; then
        owner_systemctl daemon-reload || printf 'Could not reload the user service manager.\n'
    fi
    if [[ $system_unit_changed == true && ${SUDO_UID:-0} == 0 && $manager_available == false &&
        -d /run/systemd/system && -x /usr/bin/systemctl ]] && ! system_control is-active --quiet proxywatch.service; then
        system_control daemon-reload || printf 'Could not reload the system service manager.\n'
    fi
    if ! apply_system_service "$changed"; then apply_update "$changed"; fi
    flock -u "$install_lock"
    exec {install_lock}<&-
    if "${owner_command[@]}" /bin/sh -c 'test -f "${XDG_CONFIG_HOME-$HOME/.config}/proxywatch/settings.json"'; then
        [[ $recovery == true ]] || return 0
        printf 'Proxywatch service failed to start. Run %s tui to review its settings and local ports.\n' "$binary"
        [[ $interactive == true ]] || return 0
        cleanup
        trap - EXIT HUP INT TERM
        exec "${owner_command[@]}" "$binary" tui
    fi
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
[[ ! -L $binary && ( ! -e $binary || -f $binary ) ]] || fail "Refusing to replace $binary"

umask 022
work=$(mktemp -d /tmp/proxywatch-install.XXXXXXXX)
binary_stage=''
unit_stage=''
xray_stage=''
cleanup() {
    if [[ -n $binary_stage ]]; then rm -f -- "$binary_stage"; fi
    if [[ -n $unit_stage ]]; then rm -f -- "$unit_stage"; fi
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
prepare_xray
# Releases through 0.1.2 predate the user-unit receipt and share this exact fragment.
install_unit "$work/proxywatch.service" "$unit" /usr/local/lib/proxywatch/user-unit.sha256 \
    3f3b0dc7552a938dad3e9a83e4b261f9146f2409f074ee3eaf8f4bd694567745
user_unit_changed=$installed_unit_changed
user_unit_managed=$installed_unit_managed
install_unit "$work/system.service" "$system_unit" "$unit_receipt" ''
system_unit_new=$installed_unit_new
system_unit_changed=$installed_unit_changed
system_unit_managed=$installed_unit_managed
if [[ -x $binary ]] && trusted_path "$binary" && cmp --silent -- "$work/proxywatch" "$binary"; then
    printf 'Proxywatch %s is already installed.\n' "$tag"
    finish_install "$xray_changed"
    exit 0
fi

updating=false
if [[ -f $binary ]]; then updating=true; fi
binary_stage=$(mktemp "${binary%/*}/.proxywatch.XXXXXXXX")
install -m 0755 -- "$work/proxywatch" "$binary_stage"
mv -fT -- "$binary_stage" "$binary"

if [[ $updating == true ]]; then
    printf 'Updated Proxywatch on disk to %s.\n' "$tag"
else
    printf 'Installed Proxywatch %s.\n' "$tag"
fi
finish_install "$updating"
