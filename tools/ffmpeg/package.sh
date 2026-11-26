#!/usr/bin/env bash
# Package a verified ffmpeg/ffprobe pair into a flat histv release archive.
#
# Usage: package.sh <stage-dir> <ffmpeg-source-dir> <archive-path> <linux|windows|macos>
#
# <stage-dir> already holds the stripped ffmpeg and ffprobe (.exe on Windows).
# Adds LICENSE.txt (COPYING.GPLv3 from the ffmpeg tree), BUILDINFO.txt and
# SHA256SUMS, then writes <archive-path> (.tar.gz or .zip) with no top-level
# directory. BUILDINFO.txt lists every source pin from the workflow's env block
# (values from the environment), the sources fetch.sh recorded in
# $HISTV_SOURCES, the configure line, and where the corresponding source is.
# Packaging fails if a pin that applies to this platform is missing from
# BUILDINFO.txt or differs from the workflow. Runs under macOS bash 3.2 and MSYS2.

set -euo pipefail

[ $# -eq 4 ] || { echo "usage: $0 <stage-dir> <ffmpeg-source-dir> <archive-path> <linux|windows|macos>" >&2; exit 2; }
stage=$1
ffsrc=$2
archive=$3
platform=$4
case "$platform" in
  linux | macos) exe="" ;;
  windows) exe=".exe" ;;
  *) echo "unknown platform: $platform" >&2; exit 2 ;;
esac

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@"; else shasum -a 256 "$@"; fi
}

# Pins that do not apply to a platform are left out of its BUILDINFO.
skip_pin() {
  case "$platform:$1" in
    macos:NVHEADERS_* | macos:AMF_* | macos:LIBVPL_* | macos:VULKAN_*) return 0 ;;
    macos:ZLIB_* | macos:LIBDRM_* | macos:LIBVA_*) return 0 ;;
    windows:LIBDRM_* | windows:LIBVA_*) return 0 ;;
  esac
  return 1
}

workflow="$(cd "$(dirname "$0")/../.." && pwd)/.github/workflows/build-ffmpeg.yml"
[ -f "$workflow" ] || { echo "package: missing $workflow" >&2; exit 1; }
[ -s "${HISTV_SOURCES:-}" ] || { echo "package: HISTV_SOURCES is unset or empty (fetch.sh records sources there)" >&2; exit 1; }

# Pin names and values as written in the workflow's top-level env block.
workflow_pins() {
  awk '
    /^env:/ { inenv = 1; next }
    inenv && /^[^ #]/ { exit }
    inenv && /^  [A-Z0-9_]+: / {
      key = $1; sub(/:$/, "", key)
      if (key !~ /_(TAG|COMMIT|VER|URL|SHA256)$/) next
      val = $0; sub(/^[^"]*"/, "", val); sub(/".*$/, "", val)
      print key "=" val
    }' "$workflow"
}

cp "$ffsrc/COPYING.GPLv3" "$stage/LICENSE.txt"

config=$("$stage/ffmpeg$exe" -hide_banner -version | tr -d '\r' | sed -n 's/^configuration: //p')
version=$("$stage/ffmpeg$exe" -hide_banner -version | tr -d '\r' | sed -n '1p')
compiler=$(cc --version 2>/dev/null | head -n 1 || true)

{
  echo "histv ffmpeg build ($(basename "$archive"))"
  echo
  echo "$version"
  echo "histv build revision: ${HISTV_FFMPEG_REV:-unset}"
  echo "built: $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  echo "runner: ${ImageOS:-local} ${ImageVersion:-}"
  echo "compiler: $compiler"
  if [ -n "${GITHUB_REPOSITORY:-}" ]; then
    echo "build recipe: ${GITHUB_SERVER_URL:-https://github.com}/$GITHUB_REPOSITORY/blob/${GITHUB_SHA:-}/.github/workflows/build-ffmpeg.yml"
    echo "build run: ${GITHUB_SERVER_URL:-https://github.com}/$GITHUB_REPOSITORY/actions/runs/${GITHUB_RUN_ID:-}"
  fi
  echo
  echo "Source pins (all upstream, no source patches applied):"
  workflow_pins | LC_ALL=C sort | while IFS= read -r pin; do
    key=${pin%%=*}
    skip_pin "$key" && continue
    eval "val=\${$key:-}"
    echo "  $key=$val"
  done
  echo
  echo "Sources fetched for this build (git: resolved commit; tarball: verified SHA-256):"
  while IFS="$(printf '\t')" read -r kind dest a b c; do
    case "$kind" in
      git) echo "  $(basename "$dest"): $a tag $b commit $c" ;;
      tarball) echo "  $(basename "$dest"): $b sha256 $c" ;;
    esac
  done <"$HISTV_SOURCES"
  echo
  echo "ffmpeg configuration:"
  echo "  $config"
  echo
  echo "Corresponding source: histv-ffmpeg-sources.tar, published with this"
  echo "archive (same release), holds every source above as fetched (tarballs) or"
  echo "as a git archive at the listed commit, plus the build recipe. This binary"
  echo "is built only from those sources, unmodified. LICENSE.txt is the GNU GPL v3"
  echo "under which the combined binary is distributed."
} >"$stage/BUILDINFO.txt"

# Every pin that applies here must appear in BUILDINFO.txt with the workflow's value.
missing=""
while IFS= read -r pin; do
  skip_pin "${pin%%=*}" && continue
  grep -qxF "  $pin" "$stage/BUILDINFO.txt" || missing="$missing ${pin%%=*}"
done <<EOF
$(workflow_pins)
EOF
if [ -n "$missing" ]; then
  echo "package: BUILDINFO.txt lacks or disagrees with workflow pins:$missing" >&2
  exit 1
fi

files="ffmpeg$exe ffprobe$exe LICENSE.txt BUILDINFO.txt"
(cd "$stage" && sha256 $files >SHA256SUMS)

mkdir -p "$(dirname "$archive")"
archive_abs="$(cd "$(dirname "$archive")" && pwd)/$(basename "$archive")"
rm -f "$archive_abs"
case "$archive_abs" in
  *.tar.gz) tar -C "$stage" -czf "$archive_abs" $files SHA256SUMS ;;
  *.zip) (cd "$stage" && zip -X -q "$archive_abs" $files SHA256SUMS) ;;
  *) echo "unsupported archive type: $archive" >&2; exit 2 ;;
esac

echo "packaged $archive_abs"
cat "$stage/SHA256SUMS"
