#!/usr/bin/env bash
# Fetch a pinned source: a tarball checked against its SHA-256, or a git
# commit (optionally checked against the tag that should point at it).
#
# Usage: fetch.sh <url> <sha256> <dest-dir>
#        fetch.sh --git <url> <tag|-> <commit> <dest-dir>
#
# Tarballs unpack into <dest-dir> (relative to the current directory) with the
# top-level directory stripped; the downloaded file is kept next to it. A git
# fetch checks out exactly <commit> (shallow, no submodules) and fails if <tag>
# no longer resolves to it. When HISTV_SOURCES names a file, one tab-separated
# line per fetch is appended to it for BUILDINFO.txt and the source archive:
#   tarball <dest-abs> <file-abs> <url> <sha256>
#   git     <dest-abs> <url> <tag> <commit>
# Runs under macOS bash 3.2 and MSYS2.

set -euo pipefail

record() {
  if [ -n "${HISTV_SOURCES:-}" ]; then
    (IFS="$(printf '\t')"; echo "$*") >>"$HISTV_SOURCES"
  fi
}

if [ "${1:-}" = --git ]; then
  [ $# -eq 5 ] || { echo "usage: $0 --git <url> <tag|-> <commit> <dest-dir>" >&2; exit 2; }
  url=$2
  tag=$3
  commit=$4
  dest=$5
  [ ! -e "$dest" ] || { echo "fetch: $dest already exists" >&2; exit 1; }
  git init -q "$dest"
  cd "$dest"
  git remote add origin "$url"
  if [ "$tag" != - ]; then
    git fetch -q --depth 1 origin "+refs/tags/$tag:refs/tags/$tag"
    got=$(git rev-parse "refs/tags/$tag^{commit}")
    if [ "$got" != "$commit" ]; then
      echo "tag $tag of $url resolves to $got, pinned $commit" >&2
      exit 1
    fi
  else
    git fetch -q --depth 1 origin "$commit"
  fi
  git -c advice.detachedHead=false checkout -q "$commit"
  [ "$(git rev-parse HEAD)" = "$commit" ] || { echo "checkout of $commit failed" >&2; exit 1; }
  record git "$(pwd)" "$url" "$tag" "$commit"
  echo "fetched $url ($tag) at $commit into $dest"
  exit 0
fi

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
record tarball "$(cd "$dest" && pwd)" "$(pwd)/$file" "$url" "$got"
echo "fetched $file ($got) into $dest"
