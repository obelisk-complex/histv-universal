#!/usr/bin/env bash
# End-to-end harness for a histv ffmpeg/ffprobe pair.
#
# Usage: run.sh [options]
#   --ffmpeg PATH       ffmpeg under test        (env FFMPEG, default: ffmpeg on PATH)
#   --ffprobe PATH      ffprobe under test       (env FFPROBE, default: beside ffmpeg)
#   --cli PATH          histv-cli binary         (env HISTV_CLI, default: src-tauri/target/debug/histv-cli)
#   --gen-ffmpeg PATH   ffmpeg used only to make source files
#                       (env GEN_FFMPEG, default: the ffmpeg under test)
#   --suite NAME        run only this suite; repeatable (env E2E_SUITES, space separated)
#   --list              list suites and exit
#   --keep              keep the scratch directory and print its path
#
# Runs the app's real argument builder (histv-cli) with the given ffmpeg
# against synthetic sources and checks the outputs with ffprobe. Build the CLI:
#   cd src-tauri && cargo build --no-default-features --features cli,dovi --bin histv-cli
#
# Exit status: 0 when nothing FAILed, 1 otherwise, 2 on a usage or set-up error.
# Never starts the app, never uses a display, never reads or writes the user's
# configuration: histv-cli runs with HOME and XDG_* inside the scratch directory.
# Not part of CI (the CI-side check of built archives is verify.sh).

set -u

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
SUITES_ALL="encoders size-colour animated hdr10plus"

usage() { sed -n '2,/^$/{s/^# \{0,1\}//;p}' "$0" >&2; exit 2; }
die() { echo "run.sh: $*" >&2; exit 2; }

FFMPEG=${FFMPEG:-}
FFPROBE=${FFPROBE:-}
CLI=${HISTV_CLI:-$ROOT/src-tauri/target/debug/histv-cli}
GEN_FFMPEG=${GEN_FFMPEG:-}
WANT=${E2E_SUITES:-}
KEEP=0

while [ $# -gt 0 ]; do
  case "$1" in
    --ffmpeg) FFMPEG=${2:?}; shift 2 ;;
    --ffprobe) FFPROBE=${2:?}; shift 2 ;;
    --cli) CLI=${2:?}; shift 2 ;;
    --gen-ffmpeg) GEN_FFMPEG=${2:?}; shift 2 ;;
    --suite) WANT="$WANT ${2:?}"; shift 2 ;;
    --list) echo $SUITES_ALL | tr ' ' '\n'; exit 0 ;;
    --keep) KEEP=1; shift ;;
    -h | --help) usage ;;
    *) die "unknown option: $1" ;;
  esac
done

[ -n "$FFMPEG" ] || FFMPEG=$(command -v ffmpeg) || die "no ffmpeg: pass --ffmpeg or set FFMPEG"
FFMPEG=$(readlink -f "$FFMPEG") || die "no such file: $FFMPEG"
if [ -z "$FFPROBE" ]; then FFPROBE=$(dirname "$FFMPEG")/ffprobe; fi
FFPROBE=$(readlink -f "$FFPROBE") || die "no such file: $FFPROBE"
[ -x "$FFMPEG" ] || die "not executable: $FFMPEG"
[ -x "$FFPROBE" ] || die "not executable: $FFPROBE"
[ -x "$CLI" ] || die "histv-cli not found at $CLI (build it: cd src-tauri && cargo build --no-default-features --features cli,dovi --bin histv-cli)"
CLI=$(readlink -f "$CLI")
GEN_FFMPEG=$(readlink -f "${GEN_FFMPEG:-$FFMPEG}") || die "no such generator ffmpeg"
[ -n "$WANT" ] || WANT=$SUITES_ALL
for s in $WANT; do
  [ -f "$HERE/suites/$s.sh" ] || die "unknown suite: $s (have: $SUITES_ALL)"
done

WORK=$(mktemp -d "${TMPDIR:-/tmp}/histv-e2e.XXXXXX") || die "mktemp failed"
cleanup() {
  if [ "$KEEP" = 1 ]; then echo "scratch kept: $WORK"; else rm -rf "$WORK"; fi
}
trap cleanup EXIT

# histv-cli looks for ffmpeg and ffprobe beside its own executable before any
# other place, so a private copy with symlinks makes it use exactly this pair.
mkdir -p "$WORK/bin" "$WORK/home" "$WORK/runs" "$WORK/src"
cp "$CLI" "$WORK/bin/histv-cli"
ln -s "$FFMPEG" "$WORK/bin/ffmpeg"
ln -s "$FFPROBE" "$WORK/bin/ffprobe"
CLI="$WORK/bin/histv-cli"
export HOME="$WORK/home" XDG_CONFIG_HOME="$WORK/home/config" XDG_DATA_HOME="$WORK/home/data" \
  XDG_CACHE_HOME="$WORK/home/cache" XDG_STATE_HOME="$WORK/home/state" TMPDIR="$WORK/tmp" HISTV_TMP="$WORK/tmp"
mkdir -p "$TMPDIR"
unset DISPLAY WAYLAND_DISPLAY

RESULTS="$WORK/results.tsv"
: >"$RESULTS"
SUITE=setup

# shellcheck source=lib.sh
. "$HERE/lib.sh"
# shellcheck source=gen.sh
. "$HERE/gen.sh"

echo "ffmpeg:  $FFMPEG"
"$FFMPEG" -hide_banner -version | sed -n '1p'
echo "ffprobe: $FFPROBE"
echo "histv-cli: $CLI"
[ "$GEN_FFMPEG" = "$FFMPEG" ] || echo "sources made by: $GEN_FFMPEG"

gen_sdr 256 144 10 h264 "$WORK/src/tiny.mkv" || die "could not generate the detection source"
detect_encoders
echo "app detection: $(echo $ENC_WORKS)"
echo "capabilities:  ${CAPS_LINE#\[detect\] }"
GPU=$(gpu_vendor)
echo "gpu (lowest render node): ${GPU:-none}"

for s in $WANT; do
  SUITE=$s
  echo
  echo "== $s"
  # shellcheck source=/dev/null
  . "$HERE/suites/$s.sh"
done

# --- Summary -----------------------------------------------------------------------

count() { awk -F'\t' -v s="$1" '$1 == s { n++ } END { print n + 0 }' "$RESULTS"; }
NP=$(count PASS) NF=$(count FAIL) NX=$(count XFAIL) NXP=$(count XPASS) NS=$(count SKIP)
echo
if [ "$NF" -gt 0 ]; then
  echo "failures:"
  awk -F'\t' '$1 == "FAIL" { printf "  %s: %s: %s\n", $2, $3, $4 }' "$RESULTS"
fi
echo "run.sh: $NP PASS, $NF FAIL, $NX XFAIL, $NXP XPASS, $NS SKIP"
[ "$NF" -eq 0 ]
