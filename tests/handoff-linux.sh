#!/usr/bin/env bash
# Usage: tests/handoff-linux.sh LOCAL_SYSTEMD_IMAGE LINUX_PROXYWATCH OFFICIAL_XRAY_ZIP [SCENARIO ...]
# The native Debian 12 image must contain systemd, dbus-user-session, libpam-systemd,
# Python 3 and unzip. All fixtures run in a throwaway container on loopback.
set -euo pipefail
image=${1:?missing local systemd image}
binary=$(cd "$(dirname "${2:?missing Linux proxywatch}")" && pwd -P)/$(basename "$2")
archive=$(cd "$(dirname "${3:?missing official Linux Xray ZIP}")" && pwd -P)/$(basename "$3")
tests=$(cd "$(dirname "$0")" && pwd -P)
case ${archive##*/} in
    Xray-linux-64.zip) platform=linux/amd64; checksum=8195d909f1109b8f3d99eefe401a3c451d7bf4af71f24d3815420f77e5dd2a40 ;;
    Xray-linux-arm64-v8a.zip) platform=linux/arm64; checksum=f5698bb218ada3b4022db26fafc39601c5f53b46b19eb76c9616325985807501 ;;
    *) echo 'Expected an official Xray 26.7.28 Linux ZIP filename' >&2; exit 2 ;;
esac
if [[ ! -f $binary || ! -f $archive ]]; then
    echo 'Expected a local Proxywatch binary and an official Xray ZIP file' >&2
    exit 2
fi
id=$(docker create --pull=never --platform "$platform" --network none --privileged --cgroupns=private \
    --tmpfs /run --tmpfs /run/lock --tmpfs /tmp \
    -v "$binary:/fixture/proxywatch:ro" -v "$archive:/fixture/xray.zip:ro" \
    -v "$tests/../deploy/proxywatch.service:/fixture/proxywatch.service:ro" \
    -v "$tests/../deploy/system/proxywatch.service:/fixture/proxywatch-system.service:ro" \
    -v "$tests/handoff:/fixture/test:ro" --entrypoint /bin/sh "$image" -ec '
    for unit in systemd-binfmt.service proc-sys-fs-binfmt_misc.automount proc-sys-fs-binfmt_misc.mount; do
        ln -sfn /dev/null "/etc/systemd/system/$unit"
    done
    exec /sbin/init')
trap 'docker rm -f "$id" >/dev/null' EXIT
docker start "$id" >/dev/null
docker exec -e "XRAY_SHA256=$checksum" "$id" bash -ec '
    printf "%s  /fixture/xray.zip\n" "$XRAY_SHA256" | sha256sum -c -
    timeout 45 systemctl is-system-running --wait || [[ $(systemctl is-system-running) == degraded ]]
    for unit in systemd-binfmt.service proc-sys-fs-binfmt_misc.automount; do
        [[ $(systemctl is-enabled "$unit") == masked ]]
    done
    install -Dm755 /fixture/proxywatch /usr/local/bin/proxywatch
    install -Dm644 /fixture/proxywatch.service /usr/local/lib/systemd/user/proxywatch.service
    unzip -p /fixture/xray.zip xray > /usr/local/bin/xray
    chmod 755 /usr/local/bin/xray
    systemctl start user@0.service
    exec env XDG_RUNTIME_DIR=/run/user/0 PROXYWATCH_HANDOFF_TEST=1 python3 /fixture/test/migration.py "$@"' bash "${@:4}"
