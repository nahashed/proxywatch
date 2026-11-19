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
binary=/usr/local/bin/proxywatch
unit=/usr/local/lib/systemd/user/proxywatch.service
setup
[[ $(proxywatch --version) == "proxywatch ${tag#v}" ]]
[[ $(pwch --version) == "$(proxywatch --version)" ]]
test -s "$unit"
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
rm -- /usr/local/bin/pwch
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
output=$(setup)
[[ $output == *'Running instances need a restart'* ]]
[[ $(proxywatch --version) == "proxywatch ${tag#v}" ]]
cmp /bin/true /usr/local/bin/pwch
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
printf 'PASS: %s %s\n' "$(uname -m)" "$tag"
LINUX
done
