#!/usr/bin/env bash
set -euo pipefail
assets=$(cd -- "${1:?usage: tests/install-linux.sh RELEASE_DIR vX.Y.Z}" && pwd -P)
version=${2:?missing release tag}
for platform in linux/amd64 linux/arm64; do
    docker run --rm -i --network none --platform "$platform" \
        -v "$assets:/release:ro" debian:bookworm-slim bash -se -- "$version" <<'LINUX'
set -euo pipefail
tag=$1
setup() { bash /release/install.sh --from /release --version "$tag" "$@"; }
cd /release
sha256sum -c SHA256SUMS
binary=/usr/local/bin/proxywatch
unit=/usr/local/lib/systemd/user/proxywatch.service
system_unit=/usr/local/lib/systemd/system/proxywatch.service
# An executable on the invoking user's PATH must never run under installer privileges.
mkdir /tmp/user-bin
cat > /tmp/user-bin/xray <<'XRAY'
#!/bin/sh
touch /tmp/untrusted-xray-executed
XRAY
chmod 755 /tmp/user-bin/xray
archive=proxywatch-${tag#v}-linux-$(uname -m).tar.gz
mkdir /tmp/no-xray
cp /release/SHA256SUMS "/release/$archive" /tmp/no-xray/
if output=$(PATH="/tmp/user-bin:$PATH" bash /release/install.sh --from /tmp/no-xray --version "$tag" 2>&1); then exit 1; fi
[[ $output == *'offline release directory'* && $output == *'--without-xray'* ]]
[[ ! -e /tmp/untrusted-xray-executed && ! -e "$binary" ]]
output=$(PATH="/tmp/user-bin:$PATH" setup --without-xray)
[[ $output == *'Xray installation skipped'* && ! -e /tmp/untrusted-xray-executed ]]
[[ ! -e /usr/local/lib/proxywatch/xray ]]
cat > /usr/local/bin/xray <<'XRAY'
#!/bin/sh
test "$1" = version || exit 1
id -u > /tmp/xray-probe-uid
printf 'Xray 26.7.28 (Xray, Penetrates Everything.)\n'
XRAY
chmod 755 /usr/local/bin/xray
output=$(bash /release/install.sh --from /release --version "$tag" --with-xray)
[[ $output == *'Using Xray 26.7.28 at /usr/local/bin/xray'* ]]
[[ $(cat /tmp/xray-probe-uid) == 65534 && ! -e /usr/local/lib/proxywatch/xray ]]
# A valid system engine can be reused even when the private copy is obsolete.
private_xray=/usr/local/lib/proxywatch/xray
mkdir -p "${private_xray%/*}"
sed 's/Xray 26.7.28/Xray 25.1.1/' /usr/local/bin/xray > "$private_xray"
chmod 755 "$private_xray"
engines=$(sha256sum "$private_xray" /usr/local/bin/xray)
output=$(setup)
[[ $output == *'Using Xray 26.7.28 at /usr/local/bin/xray'* ]]
[[ $(sha256sum "$private_xray" /usr/local/bin/xray) == "$engines" ]]
rm "$private_xray"
[[ $(proxywatch --version) == "proxywatch ${tag#v}" ]]
[[ $(pwch --version) == "$(proxywatch --version)" ]]
minimal_path=/sbin:/bin:/usr/sbin:/usr/bin
PATH="$minimal_path" bash --noprofile --norc -ec '
    for name in proxywatch pwch; do
        [[ $(readlink -f "$(command -v "$name")") == /usr/local/bin/proxywatch ]]
        [[ $("$name" --version) == "proxywatch ${1#v}" ]]
    done
' bash "$tag"
rm /usr/bin/proxywatch
ln -s /missing-proxywatch /usr/bin/proxywatch
setup
[[ $(readlink /usr/bin/proxywatch) == /missing-proxywatch ]]
rm /usr/bin/proxywatch
PATH="$minimal_path" bash --noprofile --norc -ec '
    ! command -v proxywatch
    bash /release/install.sh --from /release --version "$1"
    [[ $(proxywatch --version) == "proxywatch ${1#v}" ]]
' bash "$tag"
test -s "$unit"
if tar -tzf "/release/$archive" "${archive%.tar.gz}/deploy/system/proxywatch.service" >/dev/null 2>&1; then
    tar -xOzf "/release/$archive" "${archive%.tar.gz}/deploy/system/proxywatch.service" | cmp - "$system_unit"
else
    test ! -e "$system_unit"
fi
test ! -e /root/.config/proxywatch
mkdir -p /root/.config/proxywatch
cd /root/.config/proxywatch
printf '{"fixture":true}\n' > settings.json
printf 'SQLite format 3\0fixture' > state.sqlite3
state=$(sha256sum settings.json state.sqlite3)

before=$(stat -c '%i %y' "$binary" "$unit" /usr/local/bin/pwch)
[[ $(setup) == *'already installed'* ]]
[[ $(stat -c '%i %y' "$binary" "$unit" /usr/local/bin/pwch) == "$before" ]]

# An earlier release fixture can also serve as a long-running foreground process.
older=proxywatch-0.0.0-linux-$(uname -m)
mkdir -p "/tmp/previous/$older/deploy"
cp /bin/sleep "/tmp/previous/$older/proxywatch"
cp "$unit" "/tmp/previous/$older/deploy/proxywatch.service"
(cd /tmp/previous && tar -czf "$older.tar.gz" "$older" && sha256sum "$older.tar.gz" > SHA256SUMS)
rm -- /usr/local/bin/pwch /usr/bin/pwch
ln -s /missing-application /usr/bin/pwch
bash /release/install.sh --from /tmp/previous --version v0.0.0
[[ $(readlink /usr/bin/pwch) == /missing-application ]]
[[ ! -e /usr/local/bin/pwch && ! -L /usr/local/bin/pwch ]]
"$binary" 600 &
previous_pid=$!
trap 'kill "$previous_pid" 2>/dev/null || true' EXIT

original=$(sha256sum "$binary" "$unit")
mkdir /tmp/invalid
archive=proxywatch-${tag#v}-linux-$(uname -m).tar.gz
cp "/release/SHA256SUMS" "/release/$archive" /tmp/invalid/
printf invalid >> "/tmp/invalid/$archive"
if bash /release/install.sh --from /tmp/invalid --version "$tag"; then exit 1; fi
[[ $(sha256sum "$binary" "$unit") == "$original" ]]

rm -- /usr/bin/pwch
install -m 755 /bin/true /usr/local/bin/pwch
output=$(PATH="$minimal_path" setup 2>&1)
[[ $output == *'User service manager unavailable'* && $output == *'Restart any running foreground instance'* ]]
[[ $output != *'Failed to connect'* && $output != *'systemctl --user'* ]]
[[ $(proxywatch --version) == "proxywatch ${tag#v}" ]]
cmp /bin/true /usr/local/bin/pwch
[[ ! -e /usr/bin/pwch && ! -L /usr/bin/pwch ]]
before=$(stat -c '%i %y' "$binary" "$unit")
[[ $(setup) == *'already installed'* ]]
cmp /bin/true /usr/local/bin/pwch
rm -- /usr/local/bin/pwch
[[ $(setup) == *'already installed'* ]]
[[ $(readlink /usr/local/bin/pwch) == proxywatch ]]
[[ $(pwch --version) == "$(proxywatch --version)" ]]
[[ $(stat -c '%i %y' "$binary" "$unit") == "$before" ]]
kill -0 "$previous_pid"
[[ $(sha256sum settings.json state.sqlite3) == "$state" ]]
# Default repair rejects a corrupt private engine and preserves the system installation.
sed -i 's/Xray 26.7.28/Xray 25.1.1/' /usr/local/bin/xray
old_engine=$(sha256sum /usr/local/bin/xray)
mkdir /tmp/invalid-xray
cp /release/SHA256SUMS "/release/$archive" /tmp/invalid-xray/
printf invalid > /tmp/invalid-xray/Xray-linux-64.zip
printf invalid > /tmp/invalid-xray/Xray-linux-arm64-v8a.zip
if output=$(bash /release/install.sh --from /tmp/invalid-xray --version "$tag" 2>&1); then exit 1; fi
[[ $output == *'Xray archive SHA-256 mismatch'* ]]
[[ $(sha256sum /usr/local/bin/xray) == "$old_engine" && ! -e /usr/local/lib/proxywatch/xray ]]
output=$(bash /release/install.sh --from /tmp/invalid-xray --version "$tag" --without-xray)
[[ $output == *'older than 26.7.28'* && $output == *'Xray installation skipped'* && $output == *'already installed'* ]]
[[ ! -e /usr/local/lib/proxywatch/xray ]]
sed -i 's/Xray 25.1.1/Xray 26.7.28/' /usr/local/bin/xray

# Only the distributed user unit may be restarted, and only after a change.
mkdir -p /run/user
install -d -m 0700 /run/user/0
perl -MSocket -e 'my $s; socket($s, AF_UNIX, SOCK_STREAM, 0) && bind($s, sockaddr_un("/run/user/0/bus")) or die $!'
cat > /usr/bin/systemctl <<'SYSTEMD'
#!/bin/sh
test "$1" = --user || exit 2
case "$2" in
    show-environment) test "$XDG_RUNTIME_DIR" = /run/user/0 && test "$DBUS_SESSION_BUS_ADDRESS" = unix:path=/run/user/0/bus ;;
    is-active) exit 0 ;;
    show)
        case "$3" in
            --property=FragmentPath) printf '/usr/local/lib/systemd/user/proxywatch.service\n' ;;
            --property=DropInPaths) test ! -e /tmp/mock-override || printf 'override.conf\n' ;;
            *) exit 2 ;;
        esac ;;
    daemon-reload|try-restart) echo "$*" >> /tmp/systemd-actions ;;
    *) exit 2 ;;
esac
SYSTEMD
chmod 755 /usr/bin/systemctl
bash /release/install.sh --from /tmp/previous --version v0.0.0
expected_actions=$'--user daemon-reload\n--user try-restart proxywatch.service'
[[ $(cat /tmp/systemd-actions) == "$expected_actions" ]]
touch /tmp/mock-override
setup
[[ $(cat /tmp/systemd-actions) == "$expected_actions" ]]
rm /tmp/mock-override
setup
[[ $(cat /tmp/systemd-actions) == "$expected_actions" ]]

# Use a shell fixture as the newly installed command to observe the setup wizard's UID.
mkdir /tmp/wizard
cd /tmp/wizard
cat > tui <<'WIZARD'
printf 'wizard-uid=%s\n' "$(id -u)"
WIZARD
cp /bin/bash "/tmp/previous/$older/proxywatch"
(cd /tmp/previous && tar -czf "$older.tar.gz" "$older" && sha256sum "$older.tar.gz" > SHA256SUMS)
rm "$binary"
output=$(printf 'n\n' | SUDO_UID=65534 script -qec 'bash /release/install.sh --from /tmp/previous --version v0.0.0' /dev/null)
[[ $output == *'wizard-uid=65534'* ]]
# Leaving setup before Save must also reopen it on an unchanged install and on an update.
for change in unchanged update; do
    if [[ $change == update ]]; then install /bin/true "$binary"; fi
    output=$(SUDO_UID=65534 script -qec 'bash /release/install.sh --from /tmp/previous --version v0.0.0' /dev/null </dev/null)
    [[ $output == *'wizard-uid=65534'* ]]
done
output=$(SUDO_UID=65534 script -qec 'bash /release/install.sh --from /tmp/previous --version v0.0.0 --non-interactive' /dev/null </dev/null)
[[ $output == *'Setup is pending'* && $output != *'wizard-uid'* ]]

# Exercise installer decisions without a real bus. A release ELF invokes this fixture only
# when the installer delegates checked activation to the packaged Proxywatch command.
rm /run/user/0/bus
mkdir -p /run/systemd/system /tmp/system-release
cd /tmp/system-release
next=proxywatch-9.8.7-linux-$(uname -m)
mkdir -p "$next/deploy/system"
cp /bin/bash "$next/proxywatch"
cp "$unit" "$next/deploy/proxywatch.service"
printf '[Service]\nExecStart=/usr/local/bin/proxywatch run\n' > "$next/deploy/system/proxywatch.service"
tar -czf "$next.tar.gz" "$next"
sha256sum "$next.tar.gz" > SHA256SUMS
cat > activate-system-service <<'ACTIVATE'
test "$(id -u)" = 0 && test "$HOME:$XDG_CONFIG_HOME" = /root:/root/.config && test ! -t 0 || exit 1
echo activate >> /tmp/system-actions
ACTIVATE
cat > /usr/bin/systemctl <<'SYSTEM'
#!/bin/sh
if [ "$1" = --user ]; then
    case "$2" in
        show-environment|is-active) exit 0 ;;
        show) case "$3" in
            --property=FragmentPath) echo /usr/local/lib/systemd/user/proxywatch.service ;;
            --property=DropInPaths) exit 0 ;;
            *) exit 2 ;;
        esac ;;
        daemon-reload|try-restart) echo "$*" >> /tmp/system-actions ;;
        *) exit 2 ;;
    esac
    exit $?
fi
test "$1 $2" = '--system --no-ask-password' || exit 2
shift 2
case "$1" in
    is-active) test -e /tmp/system-active ;;
    is-enabled) if test -e /tmp/system-enabled; then echo enabled; else echo disabled; exit 1; fi ;;
    show)
        case "$2" in
            --property=FragmentPath) cat /tmp/system-fragment ;;
            --property=Names) echo proxywatch.service ;;
            --property=DropInPaths) if test -e /tmp/system-override; then echo example.conf; fi ;;
            --property=ActiveState,SubState) if test -e /tmp/system-retrying; then printf 'ActiveState=activating\nSubState=auto-restart\n'; elif test -e /tmp/system-failed; then printf 'ActiveState=failed\nSubState=failed\n'; fi ;;
            --property=ActiveState) if test -e /tmp/system-failed; then echo failed; fi ;;
            --property=NeedDaemonReload) echo no ;;
            *) exit 2 ;;
        esac ;;
    daemon-reload|try-restart) echo "$*" >> /tmp/system-actions ;;
    *) exit 2 ;;
esac
SYSTEM
changed_system() { bash /release/install.sh --from /tmp/system-release --version v9.8.7 --non-interactive; }
rm -f "$system_unit" /usr/local/lib/proxywatch/system-unit.sha256
echo "$system_unit" > /tmp/system-fragment
: > /tmp/system-actions
bash /release/install.sh --from /tmp/previous --version v0.0.0 --non-interactive
test ! -e "$system_unit" && test ! -s /tmp/system-actions
changed_system
[[ $(cat /tmp/system-actions) == $'daemon-reload\nactivate' ]]
system_stat=$(stat -c '%i %y' "$system_unit")
: > /tmp/system-actions
changed_system
[[ ! -s /tmp/system-actions && $(stat -c '%i %y' "$system_unit") == "$system_stat" ]]
touch /tmp/system-enabled
changed_system
[[ $(cat /tmp/system-actions) == activate ]]
touch /tmp/system-active
: > /tmp/system-actions
install /bin/true "$binary"
changed_system
[[ $(cat /tmp/system-actions) == $'daemon-reload\ntry-restart proxywatch.service' ]]
: > /tmp/system-actions
changed_system
[[ ! -s /tmp/system-actions ]]
touch /tmp/system-retrying
install /bin/true "$binary"
output=$(changed_system)
[[ $output == *'service failed to start'* && ! -s /tmp/system-actions ]]
printf '%s\n' 'echo recovery-menu-open' > tui
output=$(script -qec 'bash /release/install.sh --from /tmp/system-release --version v9.8.7' /dev/null </dev/null)
[[ $output == *'recovery-menu-open'* && ! -s /tmp/system-actions ]]
rm /tmp/system-retrying
# A stale failure in the disabled system scope must not hide a running root user unit.
rm /tmp/system-enabled /tmp/system-active
touch /tmp/system-failed
perl -MSocket -e 'my $s; socket($s, AF_UNIX, SOCK_STREAM, 0) && bind($s, sockaddr_un("/run/user/0/bus")) or die $!'
install /bin/true "$binary"
output=$(changed_system)
[[ $output == *'Restarted the active Proxywatch user service'* && $output != *'failed to start'* ]]
[[ $(cat /tmp/system-actions) == $'--user daemon-reload\n--user try-restart proxywatch.service' ]]
: > /tmp/system-actions
rm /tmp/system-failed /run/user/0/bus
touch /tmp/system-active /tmp/system-enabled
for custom in fragment override; do
    if [[ $custom == fragment ]]; then echo /etc/systemd/system/proxywatch.service > /tmp/system-fragment
    else echo "$system_unit" > /tmp/system-fragment; touch /tmp/system-override; fi
    install /bin/true "$binary"
    changed_system
    [[ ! -s /tmp/system-actions ]]
done
rm /tmp/system-override
echo '# administrator edit' >> "$system_unit"
customized=$(sha256sum "$system_unit")
install /bin/true "$binary"
output=$(changed_system)
[[ $output == *'Preserved the customized service'* && $(sha256sum "$system_unit") == "$customized" && ! -s /tmp/system-actions ]]
cp "$next/deploy/system/proxywatch.service" "$system_unit"
install /bin/true "$binary"
SUDO_UID=65534 changed_system
[[ ! -s /tmp/system-actions ]]
install /bin/true "$binary"
output=$(bash /release/install.sh --from /tmp/previous --version v0.0.0 --non-interactive)
[[ $output == *'needs a manual restart'* && ! -s /tmp/system-actions && -f $system_unit ]]
# A later official definition may replace its own recorded predecessor.
echo '# next packaged definition' >> "$next/deploy/system/proxywatch.service"
tar -czf "$next.tar.gz" "$next"
sha256sum "$next.tar.gz" > SHA256SUMS
changed_system
cmp "$next/deploy/system/proxywatch.service" "$system_unit"
[[ $(cat /tmp/system-actions) == $'daemon-reload\ntry-restart proxywatch.service' ]]
: > /tmp/system-actions
mv "$system_unit" /tmp/administrator-service
ln -s /tmp/administrator-service "$system_unit"
output=$(changed_system)
[[ $output == *'Preserved the customized service'* && $(readlink "$system_unit") == /tmp/administrator-service && ! -s /tmp/system-actions ]]
echo '# local user service' >> "$unit"
customized=$(sha256sum "$unit")
output=$(changed_system)
[[ $output == *'Preserved the customized service'* && $(sha256sum "$unit") == "$customized" ]]
printf 'PASS: %s %s\n' "$(uname -m)" "$tag"
LINUX
done
