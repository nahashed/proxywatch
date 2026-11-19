#!/usr/bin/env bash
set -euo pipefail
assets=$(cd -- "${1:?usage: tests/install-linux.sh RELEASE_DIR vX.Y.Z}" && pwd -P)
version=${2:?missing release tag}
for platform in linux/amd64 linux/arm64; do
    docker run --rm -i --network none --platform "$platform" \
        -v "$assets:/release:ro" debian:bookworm-slim bash -se -- "$version" <<'LINUX'
set -euo pipefail
tag=$1
setup() { bash /release/install.sh --from /release --version "$tag"; }
cd /release
sha256sum -c SHA256SUMS
setup
[[ $(proxywatch --version) == "proxywatch ${tag#v}" ]]
[[ $(pwch --version) == "$(proxywatch --version)" ]]
test -s /usr/local/lib/systemd/user/proxywatch.service
test ! -e /root/.config/proxywatch
mkdir -p /root/.config/proxywatch
touch /root/.config/proxywatch/keep-settings
setup
test -e /root/.config/proxywatch/keep-settings

rm -- /usr/local/bin/pwch
ln -s /missing-application /usr/bin/pwch
setup
[[ $(readlink /usr/bin/pwch) == /missing-application ]]
test ! -L /usr/local/bin/pwch
rm -- /usr/bin/pwch
install -m 755 /bin/true /usr/local/bin/pwch
setup
cmp /bin/true /usr/local/bin/pwch

original=$(sha256sum /usr/local/bin/proxywatch)
mkdir /tmp/invalid
archive=proxywatch-${tag#v}-linux-$(uname -m).tar.gz
cp "SHA256SUMS" "$archive" /tmp/invalid/
printf invalid >> "/tmp/invalid/$archive"
if bash /release/install.sh --from /tmp/invalid --version "$tag"; then exit 1; fi
[[ $(sha256sum /usr/local/bin/proxywatch) == "$original" ]]
test -e /root/.config/proxywatch/keep-settings
printf 'PASS: %s %s\n' "$(uname -m)" "$tag"
LINUX
done
