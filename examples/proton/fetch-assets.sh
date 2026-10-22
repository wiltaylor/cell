#!/bin/sh
# Download a GE-Proton release into dist/proton. It is free software and
# is not kept in the repository. Pin the version so the cell is reproducible.
set -eu
cd "$(dirname "$0")"
VERSION="${GE_PROTON_VERSION:-GE-Proton11-11}"
mkdir -p dist tmp
BASE="https://github.com/GloriousEggroll/proton-ge-custom/releases/download/$VERSION"
curl -L --fail -o "tmp/$VERSION.tar.gz" "$BASE/$VERSION-x86_64.tar.gz"
curl -L --fail -o "tmp/$VERSION.sha512sum" "$BASE/$VERSION-x86_64.sha512sum"
(cd tmp && sed "s/ .*/  $VERSION.tar.gz/" "$VERSION.sha512sum" | sha512sum -c -)
rm -rf dist/proton tmp/extract
mkdir -p tmp/extract
tar -xzf "tmp/$VERSION.tar.gz" -C tmp/extract
mv tmp/extract/* dist/proton
rm -rf tmp
echo "$VERSION ready in $(pwd)/dist/proton"
