#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 2 ]]; then
    echo 'Usage (GNU/Linux): deploy/package.sh INPUT_DIR NEW_OUTPUT_DIR' >&2
    echo 'Expected inputs: proxywatch-x86_64 and proxywatch-aarch64' >&2
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
mkdir -- "$stage/assets"
install -m 755 "$source_root/install.sh" "$stage/assets/install.sh"
for arch in x86_64 aarch64; do
    name=proxywatch-$version-linux-$arch
    mkdir -p -- "$stage/$name/deploy"
    install -m 755 "$input/proxywatch-$arch" "$stage/$name/proxywatch"
    install -m 644 "$source_root/deploy/proxywatch.service" "$stage/$name/deploy/"
    install -m 644 "$source_root"/{LICENSE,README.md,README.ru.md} "$stage/$name/"
    tar --sort=name --mtime='UTC 1970-01-01' --owner=0 --group=0 --numeric-owner \
        -C "$stage" -cf - "$name" | gzip -n >"$stage/assets/$name.tar.gz"
done
(cd -- "$stage/assets" && sha256sum -- install.sh *.tar.gz > SHA256SUMS)
mv -T --no-clobber -- "$stage/assets" "$destination"
[[ ! -e $stage/assets ]]
