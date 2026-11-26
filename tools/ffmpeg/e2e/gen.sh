#!/usr/bin/env bash
# Synthetic source generators, ffmpeg only. Sourced by run.sh.
#
# Every generator takes the output path last and prints nothing on success.
# Content is lavfi testsrc2 plus temporal noise, so the bitrate sits well above
# the 1 Mbps targets the suites use and the app chooses to transcode, not copy.

GEN_Q="-hide_banner -nostdin -v error -y"
GEN_FPS=24

# The generator ffmpeg, quiet; its own stderr (encoder banners) goes to gen.log.
_gff() { "$GEN_FFMPEG" $GEN_Q "$@" 2>>"$WORK/gen.log"; }

# PQ/BT.2020 tags, applied both as filter parameters and as encoder flags.
PQ_FILTER="format=yuv420p10le,setparams=color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc:range=tv"
SDR_PARAMS="setparams=color_primaries=bt709:color_trc=bt709:colorspace=bt709:range=tv"
PQ_FLAGS="-color_primaries bt2020 -color_trc smpte2084 -colorspace bt2020nc -color_range tv"

_gen_lavfi() { # <w> <h> <frames>
  printf 'testsrc2=s=%sx%s:r=%s:d=%s,noise=alls=25:allf=t' "$1" "$2" "$GEN_FPS" "$(awk -v n="$3" -v r="$GEN_FPS" 'BEGIN { printf "%.4f", n / r }')"
}

# gen_sdr <w> <h> <frames> <h264|hevc|av1> <out>   8-bit BT.709 source.
gen_sdr() {
  local enc
  case "$4" in
    h264) enc="-c:v libx264 -preset ultrafast -b:v 8M" ;;
    hevc) enc="-c:v libx265 -preset ultrafast -b:v 8M -x265-params log-level=error" ;;
    av1) enc="-c:v libsvtav1 -preset 12 -b:v 8M" ;;
    *) return 2 ;;
  esac
  # shellcheck disable=SC2086
  _gff -f lavfi -i "$(_gen_lavfi "$1" "$2" "$3"),format=yuv420p,$SDR_PARAMS" $enc \
    -pix_fmt yuv420p -color_primaries bt709 -color_trc bt709 -colorspace bt709 "$5"
}

# gen_pq <w> <h> <frames> <hevc|av1> <out>   10-bit BT.2020 PQ source.
gen_pq() {
  local enc
  case "$4" in
    hevc) enc="-c:v libx265 -preset ultrafast -b:v 12M -x265-params log-level=error" ;;
    av1) enc="-c:v libsvtav1 -preset 12 -b:v 12M" ;;
    *) return 2 ;;
  esac
  # shellcheck disable=SC2086
  _gff -f lavfi -i "$(_gen_lavfi "$1" "$2" "$3"),$PQ_FILTER" $enc \
    -pix_fmt yuv420p10le $PQ_FLAGS "$5"
}

# gen_gif <w> <h> <frames> <out>
gen_gif() {
  _gff -f lavfi -i "testsrc2=s=$1x$2:r=10:d=$(awk -v n="$3" 'BEGIN { printf "%.2f", n / 10 }'),format=rgb24" "$4"
}

# gen_apng <w> <h> <frames> <out>
gen_apng() {
  _gff -f lavfi -i "testsrc2=s=$1x$2:r=10:d=$(awk -v n="$3" 'BEGIN { printf "%.2f", n / 10 }'),format=rgb24" \
    -plays 0 -f apng "$4"
}

# gen_webp <w> <h> <frames> <out>
# The pinned ffmpeg has no WebP encoder, so this uses the first ffmpeg that has
# libwebp_anim: the one under test, else the system one. Returns 3 when none does.
gen_webp() {
  local f
  for f in "$GEN_FFMPEG" "$(command -v ffmpeg 2>/dev/null)"; do
    [ -n "$f" ] || continue
    if "$f" -hide_banner -encoders 2>/dev/null | grep -q libwebp_anim; then
      "$f" $GEN_Q -f lavfi -i "testsrc2=s=$1x$2:r=10:d=$(awk -v n="$3" 'BEGIN { printf "%.2f", n / 10 }'),format=rgba" \
        -c:v libwebp_anim -lossless 0 -q:v 70 -loop 0 "$4" 2>>"$WORK/gen.log"
      return $?
    fi
  done
  return 3
}

# gen_hdr10plus_json <frames> <out.json>
# x265's JSON input (LLC format). Every frame carries different luminance so a
# misordered or dropped frame shows. Profile A with a Bezier curve (the form
# the app's libx265 route accepts), one window, 400-nit target.
gen_hdr10plus_json() {
  local i=0 n=$1
  {
    printf '{"JSONInfo":{"HDR10plusProfile":"A","Version":"1.0"},"SceneInfo":['
    while [ "$i" -lt "$n" ]; do
      [ "$i" -gt 0 ] && printf ','
      printf '{"SceneFrameIndex":%d,"SceneId":0,"SequenceFrameIndex":%d,' "$i" "$i"
      printf '"BezierCurveData":{"Anchors":[256,256,256,256,256,256,256,256,256],"KneePointX":0,"KneePointY":0},'
      printf '"LuminanceParameters":{"AverageRGB":%d,"MaxScl":[%d,%d,%d],' "$((100 + i * 7))" "$((600 + i * 20))" "$((500 + i * 20))" "$((400 + i * 20))"
      printf '"LuminanceDistributions":{"DistributionIndex":[1,5,10,25,50,75,90,95,99],'
      printf '"DistributionValues":[1,3,6,20,60,150,400,600,850]}},'
      printf '"NumberOfWindows":1,"TargetedSystemDisplayMaximumLuminance":400}'
      i=$((i + 1))
    done
    printf ']}'
  } >"$2"
}

# gen_hdr10plus <w> <h> <frames> <out.mkv>
# 10-bit PQ HEVC with HDR10+ SEI, written by libx265 (dhdr10-info) with B-frames
# so display order differs from decode order. Returns 3 when x265 lacks HDR10+.
gen_hdr10plus() {
  local json="${4%.*}.json"
  gen_hdr10plus_json "$3" "$json"
  _gff -f lavfi -i "$(_gen_lavfi "$1" "$2" "$3"),$PQ_FILTER" \
    -c:v libx265 -preset ultrafast -b:v 12M -pix_fmt yuv420p10le $PQ_FLAGS \
    -x265-params "log-level=error:bframes=4:keyint=12:dhdr10-info=$json" "$4" || return 1
  hdr10plus_frames "$4" | grep -qv "^none$" || return 3
}

# hdr10plus_frames <file>: the average_maxrgb of each decoded frame, one per
# line, ordered by presentation time. Each value is unique in the generated
# source, so the list shows both dropped and misplaced metadata. A frame
# without HDR10+ side data prints "none".
hdr10plus_frames() {
  "$FFPROBE" -v error -select_streams v:0 -show_frames \
    -show_entries frame=pts,pkt_pts:frame_side_data=average_maxrgb \
    -of flat "$1" 2>/dev/null | tr -d '\r' |
    awk '
      match($0, /^frames\.frame\.[0-9]+\./) {
        f = substr($0, 14, RLENGTH - 14) + 0
        rest = substr($0, RLENGTH + 1)
        if (!(f in avg)) { avg[f] = "none"; if (f + 1 > n) n = f + 1 }
        if (rest ~ /^(pts|pkt_pts)=/ && !(f in pts)) { split(rest, a, "="); pts[f] = a[2] + 0 }
        if (rest ~ /average_maxrgb=/) { split(rest, a, "\""); split(a[2], b, "/"); avg[f] = b[1] }
      }
      END { for (f = 0; f < n; f++) printf "%d %s\n", pts[f], avg[f] }
    ' | sort -n | awk '{ print $2 }'
}
