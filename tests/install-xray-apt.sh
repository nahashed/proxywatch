#!/usr/bin/env bash
set -euo pipefail
assets=$(cd -- "${1:?usage: tests/install-xray-apt.sh RELEASE_DIR XRAY_ZIP}" && pwd -P)
archive_path=$(cd -- "$(dirname -- "${2:?missing official Xray ZIP}")" && pwd -P)/$(basename -- "$2")
test -f "$archive_path"
case ${archive_path##*/} in
    Xray-linux-64.zip) platform=linux/amd64 ;;
    Xray-linux-arm64-v8a.zip) platform=linux/arm64 ;;
    *) echo 'Expected the official Linux Xray ZIP filename' >&2; exit 2 ;;
esac
docker run --rm -i --platform "$platform" --network none -v "$assets:/release:ro" -v "$archive_path:/xray.zip:ro" debian:bookworm-slim bash -se <<'LINUX'
set -euo pipefail
if command -v unzip || command -v python3; then exit 1; fi
case "$(uname -m)" in
    x86_64) archive=Xray-linux-64.zip ;;
    aarch64) archive=Xray-linux-arm64-v8a.zip ;;
    *) exit 2 ;;
esac
mkdir /tmp/assets
cp /release/SHA256SUMS /release/*.tar.gz /tmp/assets/
ln -s /xray.zip "/tmp/assets/$archive"
version=$(sed -nE 's/^.*[[:space:]]proxywatch-([0-9.]+)-linux-aarch64\.tar\.gz$/v\1/p' /release/SHA256SUMS)
test -n "$version"

cat >/usr/bin/curl <<'CURL'
#!/bin/bash
set -eu
while [[ $1 != --output ]]; do shift; done
cp "/tmp/assets/${3##*/}" "$2"
CURL
cat >/usr/bin/apt-cache <<'CACHE'
#!/bin/sh
test "$*" = 'policy unzip' || exit 2
if [ -f /tmp/known-package ]; then echo '  Candidate: 6.0-28'; fi
CACHE
cat >/usr/bin/apt-get <<'APT'
#!/bin/sh
set -eu
test "$DEBIAN_FRONTEND" = noninteractive
echo "$*" >> /tmp/apt-calls
case "$*" in
    update) touch /tmp/known-package ;;
    'install -y --no-install-recommends --no-upgrade unzip')
        test -e /tmp/known-package
        printf '%s\n' '#!/bin/sh' 'test "$1" = -p && test "$3" = xray && cmp -s "$2" /xray.zip && cat /bin/true' > /usr/bin/unzip
        chmod 755 /usr/bin/unzip ;;
    *) exit 2 ;;
esac
APT
chmod 755 /usr/bin/{curl,apt-cache,apt-get}

if output=$(bash /release/install.sh --from /tmp/assets --version "$version" 2>&1); then exit 1; fi
[[ $output == *'Offline Xray extraction requires unzip or Python 3'* && ! -e /tmp/apt-calls ]]

bash /release/install.sh
install_command='install -y --no-install-recommends --no-upgrade unzip'
[[ $(cat /tmp/apt-calls) == "$(printf '%s\n%s' update "$install_command")" ]]
cmp /bin/true /usr/local/lib/proxywatch/xray

rm /tmp/apt-calls /usr/bin/unzip /usr/local/lib/proxywatch/xray
bash /release/install.sh
[[ $(cat /tmp/apt-calls) == "$install_command" ]]
printf 'PASS: %s offline, missing APT index, existing APT index\n' "$(uname -m)"
LINUX
