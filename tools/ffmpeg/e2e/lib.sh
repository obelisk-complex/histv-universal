#!/usr/bin/env bash
# Shared helpers for the e2e suites. Sourced by run.sh; not run directly.
#
# Result vocabulary:
#   PASS   assertion held
#   FAIL   assertion did not hold and no documented defect explains it
#   XFAIL  assertion did not hold and a documented ffmpeg/driver defect explains it
#   XPASS  an assertion marked as a known defect held (the defect may be fixed)
#   SKIP   not run: hardware, tool or ffmpeg capability absent
#
# Runs under bash 4+ on Linux. Everything lives under $WORK (a mktemp -d).

# --- Results -------------------------------------------------------------------

# Appends "STATUS<TAB>suite<TAB>name<TAB>detail" to $RESULTS and prints it.
_record() {
  local status=$1 name=$2 detail=${3:-}
  printf '%s\t%s\t%s\t%s\n' "$status" "$SUITE" "$name" "$detail" >>"$RESULTS"
  if [ -n "$detail" ]; then
    printf '  %-5s %s: %s\n' "$status" "$name" "$detail"
  else
    printf '  %-5s %s\n' "$status" "$name"
  fi
}

skip() { _record SKIP "$1" "${2:-}"; }

# verdict <name> <rc> <detail> [xfail-reason]
# rc 0 is a pass. A non-zero rc is a FAIL, or an XFAIL when a reason (the
# documented defect) is given. rc 0 with a reason is an XPASS.
verdict() {
  local name=$1 rc=$2 detail=$3 reason=${4:-}
  if [ "$rc" -eq 0 ]; then
    if [ -n "$reason" ]; then
      _record XPASS "$name" "known defect not seen: $reason"
    else
      _record PASS "$name" "$detail"
    fi
  elif [ -n "$reason" ]; then
    _record XFAIL "$name" "$detail ($reason)"
  else
    _record FAIL "$name" "$detail"
  fi
}

# expect_eq <name> <got> <want> [xfail-reason]
expect_eq() {
  if [ "$2" = "$3" ]; then
    verdict "$1" 0 "$2" "${4:-}"
  else
    verdict "$1" 1 "got '$2', want '$3'" "${4:-}"
  fi
}

# --- ffprobe -------------------------------------------------------------------

# probe_v <file> <entry>: stream-level value of the first video stream.
probe_v() {
  "$FFPROBE" -v error -select_streams v:0 -show_entries "stream=$2" \
    -of default=nw=1:nk=1 "$1" 2>/dev/null | tr -d '\r' | head -n 1
}

# frame_v <file> <entry>: value of the first decoded video frame. Reports what
# the bitstream says, not what the container says.
frame_v() {
  "$FFPROBE" -v error -select_streams v:0 -read_intervals '%+#1' -show_frames \
    -show_entries "frame=$2" -of default=nw=1:nk=1 "$1" 2>/dev/null | tr -d '\r' | head -n 1
}

# frame_count <file>: decoded video frames.
frame_count() {
  "$FFPROBE" -v error -select_streams v:0 -count_frames -show_entries stream=nb_read_frames \
    -of default=nw=1:nk=1 "$1" 2>/dev/null | tr -d '\r' | head -n 1
}

# decodes_clean <file>: full decode with no errors reported.
decodes_clean() {
  local n
  n=$("$FFMPEG" -v error -nostdin -i "$1" -map 0:v:0 -f null - 2>&1 | wc -l)
  [ "$n" -eq 0 ]
}

# --- Source generation -----------------------------------------------------------

# Sources are made with the ffmpeg under test, except animated WebP, which
# needs libwebp_anim (see gen.sh). Nothing generated is kept outside $WORK.

# --- Driving histv-cli ----------------------------------------------------------

# run_cli <tag> <input> [histv-cli args...]
# Runs the app's own batch engine, so the ffmpeg arguments are the ones the
# app builds. Sets CLI_OUT (first output file), CLI_LOG (log file), CLI_RC and
# CLI_CMD (the ffmpeg command line the app printed).
run_cli() {
  local tag=$1 input=$2
  shift 2
  local dir="$WORK/runs/$tag"
  mkdir -p "$dir/out"
  CLI_LOG="$dir/cli.log"
  # --overwrite yes: no prompt. --fallback no: a failing hardware encode is a
  # failure here, not a silent software retry.
  timeout "${E2E_TIMEOUT:-600}" "$CLI" --output "$dir/out" --overwrite yes --fallback no \
    --log-level verbose "$@" "$input" >"$CLI_LOG" 2>&1 </dev/null
  CLI_RC=$?
  CLI_OUT=$(find "$dir/out" -maxdepth 1 -type f \( -name '*.mkv' -o -name '*.mp4' \) | head -n 1)
  # shellcheck disable=SC2034  # read by the suites
  CLI_CMD=$(grep -m1 '^  CMD: ' "$CLI_LOG" | sed 's/^  CMD: //')
  return 0
}

# cli_ok <name>: the run exited 0 and produced a file that decodes cleanly.
# Returns 1 (after recording FAIL) otherwise, so callers can stop early.
cli_ok() {
  if [ "$CLI_RC" -ne 0 ] || [ -z "$CLI_OUT" ]; then
    verdict "$1: encode" 1 "histv-cli exit $CLI_RC, output '${CLI_OUT:-none}'; $(grep -E 'ERROR|WARNING|FAIL|failed' "$CLI_LOG" | tail -n 2 | tr '\n' ' ')"
    return 1
  fi
  if ! decodes_clean "$CLI_OUT"; then
    verdict "$1: encode" 1 "output does not decode cleanly"
    return 1
  fi
  return 0
}

# --- Encoder availability --------------------------------------------------------

# Fills ENC_WORKS (encoders the app's own detection accepted) and CAPS_LINE
# from one --dry-run, so the harness gates on exactly what the app decided.
detect_encoders() {
  local log="$WORK/detect.log"
  timeout 180 "$CLI" --dry-run --log-level verbose --output "$WORK/detect-out" "$WORK/src/tiny.mkv" >"$log" 2>&1 </dev/null
  ENC_WORKS=" $(sed -n 's/^\[detect\] \([a-z0-9_]*\) (HW) - works$/\1/p; s/^\[detect\] \([a-z0-9_]*\) (SW) - available$/\1/p' "$log" | tr '\n' ' ')"
  CAPS_LINE=$(grep -m1 '^\[detect\] Native Dolby Vision' "$log")
}

enc_works() { case "$ENC_WORKS" in *" $1 "*) return 0 ;; *) return 1 ;; esac; }

# render_node: the node the app uses (the lowest-numbered one).
render_node() { ls /dev/dri/renderD* 2>/dev/null | sort | head -n 1; }

# enc_usable <encoder>: the app's detection accepted it. av1_vulkan is not in
# the app's priority tables (it is never chosen automatically), so detection
# does not test it; it gets the same one-frame encode with the app's device set-up.
enc_usable() {
  local node
  case "$1" in
    av1_vulkan)
      node=$(render_node)
      [ -n "$node" ] || return 1
      "$FFMPEG" -hide_banner -nostdin -v error -init_hw_device "drm=dr:$node" \
        -init_hw_device vulkan=vk@dr -filter_hw_device vk \
        -f lavfi -i "color=black:s=256x256:d=0.1:r=25" -frames:v 1 \
        -vf format=nv12,hwupload -c:v av1_vulkan -f null - >/dev/null 2>&1 ;;
    *) enc_works "$1" ;;
  esac
}

# caps_yes <label>: the app's capability line says "<label> yes".
caps_yes() { printf '%s' "$CAPS_LINE" | grep -q "$1 yes"; }

# why_not <encoder>: one-line reason an encoder is not run on this host.
why_not() {
  case "$1" in
    *_d3d12va) echo "Direct3D 12 encoders need Windows with a D3D12 video-encode device" ;;
    *_vaapi) if [ -z "$(ls /dev/dri/renderD* 2>/dev/null)" ]; then echo "no /dev/dri render node"; else echo "app detection: VAAPI encoder not usable on this host"; fi ;;
    *_vulkan) echo "no Vulkan video-encode device, or encoder not in this ffmpeg" ;;
    *_qsv) echo "no Intel QSV device" ;;
    *_nvenc) echo "no NVIDIA device" ;;
    *_amf) echo "AMF needs the AMD proprietary stack (Windows, or amdgpu-pro on Linux)" ;;
    *) echo "encoder not available" ;;
  esac
}

# size_defect <encoder> <container|decoded>: the documented defect that makes an encoder's picture
# size wrong in the bitstream (empty when none applies on this host). Used as
# the XFAIL reason; anything else that is wrong stays a FAIL.
size_defect() {
  case "$1" in
    av1_vulkan) echo "known: n8.1.3 av1_vulkan writes the 16-aligned surface size as the AV1 frame size, no render size" ;;
    av1_vaapi) [ "$2" = decoded ] && [ "$GPU" = amd ] && echo "known: av1_vaapi on AMD VCN (Mesa radeonsi) codes a taller picture, e.g. 2 rows at 360p and 1080p" ;;
  esac
  return 0
}

# gpu_vendor: PCI vendor of the lowest render node (the one the app uses).
gpu_vendor() {
  local node v
  node=$(ls /dev/dri/renderD* 2>/dev/null | sort | head -n 1)
  [ -n "$node" ] || return 0
  v=$(cat "/sys/class/drm/$(basename "$node")/device/vendor" 2>/dev/null)
  case "$v" in
    0x1002) echo amd ;;
    0x8086) echo intel ;;
    0x10de) echo nvidia ;;
    *) echo "${v:-unknown}" ;;
  esac
}
