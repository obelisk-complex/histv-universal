#!/usr/bin/env bash
# Download a pinned source tarball, check its SHA-256, and unpack it.
#
# Usage: fetch.sh <url> <sha256> <dest-dir>
#
# Unpacks into <dest-dir> (relative to the current directory) with the
# tarball's top-level directory stripped. Runs under macOS bash 3.2 and MSYS2.

set -euo pipefail

[ $# -eq 3 ] || { echo "usage: $0 <url> <sha256> <dest-dir>" >&2; exit 2; }
url=$1
want=$2
dest=$3
file=$(basename "$url")

curl -fsSL --retry 3 --retry-delay 5 -o "$file" "$url"

if command -v sha256sum >/dev/null 2>&1; then
  got=$(sha256sum "$file" | awk '{ print $1 }')
else
  got=$(shasum -a 256 "$file" | awk '{ print $1 }')
fi
if [ "$got" != "$want" ]; then
  echo "checksum mismatch for $file: expected $want, got $got" >&2
  exit 1
fi

mkdir -p "$dest"
tar -xf "$file" -C "$dest" --strip-components=1
echo "fetched $file ($got) into $dest"
