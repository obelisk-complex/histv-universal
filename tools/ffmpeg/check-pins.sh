#!/usr/bin/env bash
# Fail when the pinned histv ffmpeg release differs between the places that
# consume it.
#
# Usage: check-pins.sh [--allow-placeholders] [repo-root]
#
# The release tag, the four asset names and their sha256 values are pinned in:
#   src-tauri/src/ffmpeg.rs              (mod histv_release + Candidate::histv calls)
#   .github/workflows/build-platform.yml (env FFMPEG_HISTV_* + the per-label asset case)
#   com.histv.encoder.yml                (ffmpeg module: url and sha256; Linux only)
# Each sha256 is bound to its asset in every file, not just compared by name:
# a swapped variable, a swapped constant or a renamed asset fails. ffmpeg.rs is
# the reference. A pin that cannot be found counts as a failure, so a renamed
# constant or reformatted file cannot make the check pass empty.
#
# All-zero sha256 values (an unreleased build) fail unless --allow-placeholders
# is given or HISTV_ALLOW_PLACEHOLDER_PINS=1 is set.

set -u

ALLOW=${HISTV_ALLOW_PLACEHOLDER_PINS:-0}
if [ "${1:-}" = "--allow-placeholders" ]; then
  ALLOW=1
  shift
fi
ROOT=${1:-$(cd "$(dirname "$0")/../.." && pwd)}
RS="$ROOT/src-tauri/src/ffmpeg.rs"
PLATFORM="$ROOT/.github/workflows/build-platform.yml"
FLATPAK="$ROOT/com.histv.encoder.yml"

for f in "$RS" "$PLATFORM" "$FLATPAK"; do
  [ -f "$f" ] || { echo "check-pins: FAIL: missing $f" >&2; exit 1; }
done

TMP=$(mktemp -d "${TMPDIR:-/tmp}/histv-pins.XXXXXX") || exit 1
trap 'rm -rf "$TMP"' EXIT

# Expected "key label asset" table; the label is build-platform.yml's.
cat >"$TMP/expected" <<'EOF'
LINUX_X64 linux histv-ffmpeg-linux-x64.tar.gz
WINDOWS_X64 windows histv-ffmpeg-windows-x64.zip
MACOS_ARM64 macos-arm64 histv-ffmpeg-macos-arm64.zip
MACOS_X64 macos-x64 histv-ffmpeg-macos-x64.zip
EOF

# Extractors. Each prints "KIND ARG ..." lines.

# CONST <name> <value>      pub const in mod histv_release
# BIND <asset> <const>      Candidate::histv("asset", ..., histv_release::SHA256_X)
awk '
  /mod histv_release/ { inmod = 1 }
  inmod && /pub const (TAG|SHA256_[A-Z0-9_]+)/ { name = $3; sub(/:$/, "", name); want = 1 }
  inmod && want && match($0, /"[^"]*"/) { print "CONST", name, substr($0, RSTART + 1, RLENGTH - 2); want = 0 }
  inmod && /^}/ { inmod = 0 }
  /Candidate::histv\(/ { inc = 1; asset = ""; next }
  inc && asset == "" && match($0, /"[^"]*"/) { asset = substr($0, RSTART + 1, RLENGTH - 2) }
  inc && match($0, /histv_release::SHA256_[A-Z0-9_]+/) {
    print "BIND", asset, substr($0, RSTART + 15, RLENGTH - 15); inc = 0
  }
' "$RS" >"$TMP/rs"

# VAR <name> <value>        env pin (name without the FFMPEG_HISTV_ prefix)
# CASE <label> <asset>      ASSET="..." in the label case
# SHAVAR <label> <name>     SHA256="$FFMPEG_HISTV_..." in the same case arm
awk '
  /^  FFMPEG_HISTV_(TAG|SHA256_[A-Z0-9_]+):/ {
    name = $1; sub(/:$/, "", name); sub(/^FFMPEG_HISTV_/, "", name)
    if (match($0, /"[^"]*"/)) print "VAR", name, substr($0, RSTART + 1, RLENGTH - 2)
  }
  /^ +(linux|windows|macos-arm64|macos-x64)\)$/ { lab = $1; sub(/\)$/, "", lab) }
  /^ +ASSET="/ { a = $1; sub(/^ASSET="/, "", a); sub(/"$/, "", a); print "CASE", lab, a }
  /^ +SHA256="\$FFMPEG_HISTV_/ { v = $1; sub(/^SHA256="\$FFMPEG_HISTV_/, "", v); sub(/"$/, "", v); print "SHAVAR", lab, v }
' "$PLATFORM" >"$TMP/platform"

# TAG <tag>, ASSET <asset>, SHA <value>
awk '
  /^ +url:.*\/releases\/download\/[^\/]+\/histv-ffmpeg-[^\/]+$/ {
    u = $2; n = split(u, p, "/"); print "TAG", p[n - 1]; print "ASSET", p[n]; want = 1; next
  }
  want && /^ +sha256:/ { v = $2; gsub(/["'"'"']/, "", v); print "SHA", v; want = 0 }
' "$FLATPAK" >"$TMP/flatpak"

# get <file> <kind> <arg>: third field of the first "<kind> <arg> <value>" line.
get() { awk -v k="$2" -v a="$3" '$1 == k && $2 == a { print $3; exit }' "$TMP/$1"; }
# get1 <file> <kind>: value of a one-argument line.
get1() { awk -v k="$2" '$1 == k { print $2; exit }' "$TMP/$1"; }

NFAIL=0
fail() { NFAIL=$((NFAIL + 1)); echo "FAIL: $1" >&2; }

# --- Reference: ffmpeg.rs ---------------------------------------------------

TAG=$(get rs CONST TAG)
if [ -z "$TAG" ]; then
  fail "TAG not found in ffmpeg.rs"
elif ! printf '%s' "$TAG" | grep -Eq '^ffmpeg-n[0-9.]+-histv\.[0-9]+$'; then
  fail "TAG '$TAG' in ffmpeg.rs is not ffmpeg-n<version>-histv.<rev>"
fi

while read -r key label asset; do
  ref=$(get rs CONST "SHA256_$key")
  if [ -z "$ref" ]; then
    fail "SHA256_$key not found in ffmpeg.rs"
    continue
  fi
  printf '%s' "$ref" | grep -Eq '^[0-9a-f]{64}$' || fail "SHA256_$key '$ref' in ffmpeg.rs is not 64 lowercase hex digits"
  if [ "$ALLOW" != 1 ] && printf '%s' "$ref" | grep -Eq '^0{64}$'; then
    fail "SHA256_$key is still the all-zero placeholder (use --allow-placeholders for an unreleased build)"
  fi

  # ffmpeg.rs: the asset must be bound to its own constant.
  bound=$(get rs BIND "$asset")
  if [ -z "$bound" ]; then
    fail "ffmpeg.rs has no Candidate::histv for $asset"
  elif [ "$bound" != "SHA256_$key" ]; then
    fail "ffmpeg.rs binds $asset to $bound, expected SHA256_$key"
  fi

  # build-platform.yml: label -> asset -> variable -> value.
  pa=$(awk -v l="$label" '$1 == "CASE" && $2 == l { print $3; exit }' "$TMP/platform")
  pv=$(awk -v l="$label" '$1 == "SHAVAR" && $2 == l { print $3; exit }' "$TMP/platform")
  if [ -z "$pa" ]; then
    fail "build-platform.yml has no ASSET for label $label"
  elif [ "$pa" != "$asset" ]; then
    fail "build-platform.yml label $label uses asset $pa, expected $asset"
  fi
  if [ -z "$pv" ]; then
    fail "build-platform.yml has no SHA256 variable for label $label"
  elif [ "$pv" != "SHA256_$key" ]; then
    fail "build-platform.yml label $label uses FFMPEG_HISTV_$pv, expected FFMPEG_HISTV_SHA256_$key"
  fi
  got=$(get platform VAR "SHA256_$key")
  if [ -z "$got" ]; then
    fail "FFMPEG_HISTV_SHA256_$key not found in build-platform.yml"
  elif [ "$got" != "$ref" ]; then
    fail "SHA256_$key differs: ffmpeg.rs=$ref build-platform.yml=$got"
  fi

  # Flatpak manifest: Linux only.
  if [ "$key" = LINUX_X64 ]; then
    ma=$(get1 flatpak ASSET)
    ms=$(get1 flatpak SHA)
    [ "$ma" = "$asset" ] || fail "com.histv.encoder.yml url asset is '$ma', expected $asset"
    if [ -z "$ms" ]; then
      fail "sha256 not found in com.histv.encoder.yml"
    elif [ "$ms" != "$ref" ]; then
      fail "SHA256_$key differs: ffmpeg.rs=$ref com.histv.encoder.yml=$ms"
    fi
  fi
done <"$TMP/expected"

# --- Tag in the other files --------------------------------------------------

ptag=$(awk '$1 == "VAR" && $2 == "TAG" { print $3; exit }' "$TMP/platform")
mtag=$(get1 flatpak TAG)
if [ -z "$ptag" ]; then fail "FFMPEG_HISTV_TAG not found in build-platform.yml"; elif [ "$ptag" != "$TAG" ]; then fail "TAG differs: ffmpeg.rs=$TAG build-platform.yml=$ptag"; fi
if [ -z "$mtag" ]; then fail "release tag not found in com.histv.encoder.yml url"; elif [ "$mtag" != "$TAG" ]; then fail "TAG differs: ffmpeg.rs=$TAG com.histv.encoder.yml=$mtag"; fi

# --- build-platform.yml download and verify lines use the bound values --------

grep -qF 'releases/download/${FFMPEG_HISTV_TAG}/${ASSET}"' "$PLATFORM" || fail 'build-platform.yml download URL is not .../${FFMPEG_HISTV_TAG}/${ASSET}'
[ "$(grep -cF '"${SHA256}  ${ASSET}" |' "$PLATFORM")" -ge 2 ] || fail 'build-platform.yml does not check "${SHA256}  ${ASSET}" with sha256sum/shasum'

if [ "$NFAIL" -ne 0 ]; then
  echo "check-pins: FAIL ($NFAIL problem(s))" >&2
  exit 1
fi

echo "check-pins: OK, $TAG: tag, asset names and sha256 bindings agree across ffmpeg.rs, build-platform.yml and com.histv.encoder.yml"
