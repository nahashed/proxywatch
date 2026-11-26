#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 2 || $# -gt 3 ]]; then
    echo 'Usage (GNU/Linux): deploy/package.sh INPUT_DIR NEW_OUTPUT_DIR [PREPARED_LICENSE_DIR]' >&2
    echo 'Expected inputs: proxywatch-x86_64 and proxywatch-aarch64' >&2
    echo 'Generate notices offline with Cargo and Python 3, or pass the output of deploy/collect-licenses.py.' >&2
    exit 2
fi

source_root=$(cd -- "$(dirname -- "$0")/.." && pwd -P)
input=$(cd -- "$1" && pwd -P)
version=$(awk '/^\[package\]$/ { package=1; next } /^\[/ { package=0 } package && /^version = "/ { print $3; exit }' "$source_root/Cargo.toml")
version=${version//\"/}
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo 'Expected a stable Cargo package version' >&2; exit 1; }
for arch in x86_64 aarch64; do
    [[ -s $input/proxywatch-$arch ]] || { echo "Missing Linux binary for $arch" >&2; exit 1; }
done

# Stage beside the destination so an interrupted build never exposes a partial release.
parent=$(cd -- "$(dirname -- "$2")" && pwd -P)
destination=$parent/$(basename -- "$2")
[[ ! -e $destination && ! -L $destination ]] || { echo 'Output already exists' >&2; exit 1; }
stage=$(mktemp -d -- "$parent/.proxywatch-package.XXXXXXXX")
trap 'rm -rf -- "$stage"' EXIT
if [[ $# == 3 ]]; then
    notices=$(cd -- "$3" && pwd -P)
else
    notices=$stage/notices
    python3 "$source_root/deploy/collect-licenses.py" "$notices"
fi
[[ -s $notices/index.json && -s $notices/cargo-inputs.sha256 && -s $notices/SHA256SUMS ]] ||
    { echo 'Missing prepared dependency license index or checksums' >&2; exit 1; }
(cd -- "$source_root" && sha256sum --strict --status -c "$notices/cargo-inputs.sha256") ||
    { echo 'Regenerate dependency notices for the current Cargo inputs' >&2; exit 1; }
(cd -- "$notices" && sha256sum --strict --status -c SHA256SUMS) ||
    { echo 'Dependency license checksum mismatch' >&2; exit 1; }
# Copy only the collector's listed notices, even if someone placed an unrelated
# file alongside a prepared bundle.
mkdir -- "$stage/THIRD-PARTY-LICENSES"
while read -r digest path; do
    [[ $digest =~ ^[0-9a-f]{64}$ && $path != /* && /$path/ != */../* ]]
    install -D -m 644 -- "$notices/$path" "$stage/THIRD-PARTY-LICENSES/$path"
done < "$notices/SHA256SUMS"
install -m 644 -- "$notices/SHA256SUMS" "$stage/THIRD-PARTY-LICENSES/"
mkdir -- "$stage/assets"
install -m 755 "$source_root/install.sh" "$stage/assets/install.sh"
for arch in x86_64 aarch64; do
    name=proxywatch-$version-linux-$arch
    mkdir -p -- "$stage/$name/deploy/system"
    install -m 755 "$input/proxywatch-$arch" "$stage/$name/proxywatch"
    install -m 644 "$source_root/deploy/proxywatch.service" "$stage/$name/deploy/"
    install -m 644 "$source_root/deploy/system/proxywatch.service" "$stage/$name/deploy/system/"
    install -m 644 "$source_root"/{LICENSE,README.md,README.ru.md} "$stage/$name/"
    cp -R -- "$stage/THIRD-PARTY-LICENSES" "$stage/$name/"
    tar --sort=name --mtime='UTC 1970-01-01' --owner=0 --group=0 --numeric-owner \
        -C "$stage" -cf - "$name" | gzip -n >"$stage/assets/$name.tar.gz"
done
(cd -- "$stage/assets" && sha256sum -- install.sh *.tar.gz > SHA256SUMS)
mv -T --no-clobber -- "$stage/assets" "$destination"
[[ ! -e $stage/assets ]]
