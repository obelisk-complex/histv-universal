#!/usr/bin/env bash
# Package a verified ffmpeg/ffprobe pair into a flat histv release archive.
#
# Usage: package.sh <stage-dir> <ffmpeg-source-dir> <archive-path> <linux|windows|macos>
#
# <stage-dir> already holds the stripped ffmpeg and ffprobe (.exe on Windows).
# Adds LICENSE.txt (COPYING.GPLv3 from the ffmpeg tree), BUILDINFO.txt (source
# pins from the environment, the configure line, where to get the source) and
# SHA256SUMS, then writes <archive-path> (.tar.gz or .zip) with no top-level
# directory. Runs under macOS bash 3.2 and MSYS2.

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
    windows:ZLIB_* | windows:LIBDRM_* | windows:LIBVA_*) return 0 ;;
  esac
  return 1
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
  env | LC_ALL=C sort | grep -E '^[A-Z0-9]+_(TAG|COMMIT|VER|URL|SHA256)=' | while IFS= read -r line; do
    skip_pin "$line" || echo "  $line"
  done
  echo
  echo "Git sources (checked out at the tag or commit above):"
  echo "  FFMPEG_TAG      https://github.com/FFmpeg/FFmpeg.git"
  echo "  X264_COMMIT     https://code.videolan.org/videolan/x264.git"
  echo "  X265_VER        https://download.videolan.org/pub/videolan/x265/x265_<X265_VER>.tar.gz"
  echo "  SVTAV1_TAG      https://gitlab.com/AOMediaCodec/SVT-AV1.git"
  echo "  ZIMG_TAG        https://github.com/sekrit-twc/zimg.git (with submodules)"
  if [ "$platform" != macos ]; then
    echo "  NVHEADERS_TAG   https://github.com/FFmpeg/nv-codec-headers.git"
    echo "  AMF_TAG         https://github.com/GPUOpen-LibrariesAndSDKs/AMF.git"
    echo "  LIBVPL_TAG      https://github.com/intel/libvpl.git"
    echo "  VULKAN_HEADERS_TAG https://github.com/KhronosGroup/Vulkan-Headers.git"
  fi
  echo "  Tarball sources are the *_URL entries, checked against *_SHA256."
  echo
  echo "ffmpeg configuration:"
  echo "  $config"
  echo
  echo "Corresponding source: this binary is built only from the upstream sources"
  echo "listed above, unmodified, using the build recipe linked above. LICENSE.txt"
  echo "is the GNU GPL v3 under which the combined binary is distributed."
} >"$stage/BUILDINFO.txt"

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
