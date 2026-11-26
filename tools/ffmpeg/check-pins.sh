#!/usr/bin/env bash
# Fail when the pinned histv ffmpeg release differs between the places that
# consume it.
#
# Usage: check-pins.sh [repo-root]
#
# Compares the release tag and the per-platform sha256 values in:
#   src-tauri/src/ffmpeg.rs            (mod histv_release: TAG, SHA256_*)
#   .github/workflows/build-platform.yml (env: FFMPEG_HISTV_TAG, FFMPEG_HISTV_SHA256_*)
#   com.histv.encoder.yml              (ffmpeg module: url tag, sha256; Linux only)
# ffmpeg.rs is the reference. A pin that cannot be found counts as a failure,
# so a renamed constant or reformatted file cannot make the check pass empty.

set -u

ROOT=${1:-$(cd "$(dirname "$0")/../.." && pwd)}
RS="$ROOT/src-tauri/src/ffmpeg.rs"
PLATFORM="$ROOT/.github/workflows/build-platform.yml"
FLATPAK="$ROOT/com.histv.encoder.yml"

for f in "$RS" "$PLATFORM" "$FLATPAK"; do
  [ -f "$f" ] || { echo "check-pins: FAIL: missing $f" >&2; exit 1; }
done

TMP=$(mktemp -d "${TMPDIR:-/tmp}/histv-pins.XXXXXX") || exit 1
trap 'rm -rf "$TMP"' EXIT

# Each extractor prints "KEY VALUE" lines (KEY is TAG or SHA256_<PLATFORM>).

awk '
  /mod histv_release/ { inmod = 1 }
  inmod && /pub const (TAG|SHA256_[A-Z0-9_]+)/ { name = $3; sub(/:$/, "", name); want = 1 }
  inmod && want && match($0, /"[^"]*"/) { print name, substr($0, RSTART + 1, RLENGTH - 2); want = 0 }
  inmod && /^}/ { exit }
' "$RS" >"$TMP/rs"

awk '
  /^  FFMPEG_HISTV_(TAG|SHA256_[A-Z0-9_]+):/ {
    name = $1; sub(/:$/, "", name); sub(/^FFMPEG_HISTV_/, "", name)
    if (match($0, /"[^"]*"/)) print name, substr($0, RSTART + 1, RLENGTH - 2)
  }
' "$PLATFORM" >"$TMP/platform"

awk '
  /^ +url:.*\/releases\/download\/[^\/]+\/histv-ffmpeg-linux-x64\.tar\.gz/ {
    u = $0; sub(/.*\/releases\/download\//, "", u); sub(/\/.*/, "", u)
    print "TAG", u; want = 1; next
  }
  want && /^ +sha256:/ {
    v = $2; gsub(/["'"'"']/, "", v); print "SHA256_LINUX_X64", v; want = 0
  }
' "$FLATPAK" >"$TMP/flatpak"

# value <file> <key>: first value recorded for the key, or empty.
value() { awk -v k="$2" '$1 == k { print $2; exit }' "$TMP/$1"; }

KEYS="TAG SHA256_LINUX_X64 SHA256_WINDOWS_X64 SHA256_MACOS_ARM64 SHA256_MACOS_X64"
NFAIL=0
fail() { NFAIL=$((NFAIL + 1)); echo "FAIL: $1" >&2; }

for key in $KEYS; do
  ref=$(value rs "$key")
  if [ -z "$ref" ]; then
    fail "$key not found in src-tauri/src/ffmpeg.rs"
    continue
  fi
  case "$key" in
    TAG) printf '%s' "$ref" | grep -Eq '^ffmpeg-n[0-9.]+-histv\.[0-9]+$' || fail "TAG '$ref' in ffmpeg.rs is not ffmpeg-n<version>-histv.<rev>" ;;
    *) printf '%s' "$ref" | grep -Eq '^[0-9a-f]{64}$' || fail "$key '$ref' in ffmpeg.rs is not 64 lowercase hex digits" ;;
  esac

  for other in platform flatpak; do
    # The Flatpak manifest only consumes the Linux archive.
    if [ "$other" = flatpak ]; then
      case "$key" in TAG | SHA256_LINUX_X64) ;; *) continue ;; esac
    fi
    got=$(value "$other" "$key")
    if [ -z "$got" ]; then
      fail "$key not found in $other"
    elif [ "$got" != "$ref" ]; then
      fail "$key differs: ffmpeg.rs=$ref $other=$got"
    fi
  done
done

if [ "$NFAIL" -ne 0 ]; then
  echo "check-pins: FAIL ($NFAIL problem(s))" >&2
  exit 1
fi

echo "check-pins: OK, $(value rs TAG) agrees across ffmpeg.rs, build-platform.yml and com.histv.encoder.yml"
if grep -q '^SHA256_.* 0\{64\}$' "$TMP/rs"; then
  echo "check-pins: note: placeholder (all-zero) sha256 values are still pinned" >&2
fi
