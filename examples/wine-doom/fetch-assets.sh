#!/bin/sh
# Download the Chocolate Doom Windows build and the Freedoom data into dist/.
# Both are free software; they are not kept in the repository.
set -eu
cd "$(dirname "$0")"
mkdir -p dist/chocolate-doom dist/freedoom tmp
curl -L --fail -o tmp/choco.zip https://github.com/chocolate-doom/chocolate-doom/releases/download/chocolate-doom-3.1.0/chocolate-doom-3.1.0-win32.zip
curl -L --fail -o tmp/freedoom.zip https://github.com/freedoom/freedoom/releases/download/v0.13.0/freedoom-0.13.0.zip
unzip -q -o tmp/choco.zip -d tmp/choco
unzip -q -o tmp/freedoom.zip -d tmp/freedoom
cp -r tmp/choco/*/* dist/chocolate-doom/ 2>/dev/null || cp -r tmp/choco/* dist/chocolate-doom/
cp tmp/freedoom/*/freedoom1.wad tmp/freedoom/*/freedoom2.wad dist/freedoom/
rm -rf tmp
echo "assets ready in $(pwd)/dist"
