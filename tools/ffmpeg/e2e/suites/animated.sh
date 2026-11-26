#!/usr/bin/env bash
# Suite: animated images (GIF, APNG, animated WebP). Sourced by run.sh.
#
# The app always transcodes these (they cannot be stream-copied). GIF and APNG
# are decoded by ffmpeg; animated WebP is composited by the app's own RIFF
# parser and piped to a second ffmpeg as raw RGBA. Odd dimensions are padded to
# even ones. Hardware encoders need the frames uploaded, which is the path a
# forced --encoder takes for an image source.
# Checked per (format, size, encoder): the encode succeeds and decodes, the
# output is HEVC or H.264 at the even-padded size (container and decoded), and
# it plays for as long as the source (frame count is not compared: the WebP
# route re-times to a 50 fps grid, so the frame count is 5x).
# Sizes start above 130x128, the smallest picture VAAPI hardware on AMD VCN
# accepts. APNG runs once, with the first software encoder.

FRAMES=20
SRC_SECONDS=2.0
SIZES="320x180 321x181"

even() { echo $((($1 + 1) / 2 * 2)); }

# duration_ok <file>: container duration within 0.15 s of the source's.
duration_ok() {
  local d
  d=$("$FFPROBE" -v error -show_entries format=duration -of default=nw=1:nk=1 "$1" 2>/dev/null | head -n 1)
  awk -v d="${d:-0}" -v s="$SRC_SECONDS" 'BEGIN { x = d - s; if (x < 0) x = -x; exit !(x <= 0.15) }'
}

for sz in $SIZES; do
  w=${sz%x*}
  h=${sz#*x}
  gen_gif "$w" "$h" $FRAMES "$WORK/src/anim-$sz.gif" || skip "gif $sz" "cannot make a GIF with $GEN_FFMPEG"
  gen_apng "$w" "$h" $FRAMES "$WORK/src/anim-$sz.apng" || skip "apng $sz" "cannot make an APNG with $GEN_FFMPEG"
  gen_webp "$w" "$h" $FRAMES "$WORK/src/anim-$sz.webp"
  [ $? -eq 0 ] || rm -f "$WORK/src/anim-$sz.webp"
done

if ! ls "$WORK"/src/anim-*.webp >/dev/null 2>&1; then
  skip "webp" "no ffmpeg with libwebp_anim to make a source (the pinned build has no WebP encoder; pass --gen-ffmpeg or install one)"
fi

for spec in "hevc libx265 hevc_vaapi hevc_vulkan" "h264 libx264 h264_vaapi h264_vulkan"; do
  set -- $spec
  fam=$1
  shift
  for enc in "$@"; do
    if ! enc_usable "$enc"; then
      skip "$enc" "$(why_not "$enc")"
      continue
    fi
    for src in "$WORK"/src/anim-*.*; do
      base=$(basename "$src")
      ext=${base##*.}
      if [ "$ext" = apng ]; then
        [ "$enc" = libx265 ] && [ "${base#anim-}" = "${SIZES%% *}.apng" ] || continue
      fi
      sz=${base#anim-}
      sz=${sz%.*}
      w=${sz%x*}
      h=${sz#*x}
      want="$(even "$w")x$(even "$h")"
      name="$enc $ext $sz"
      run_cli "an-$enc-$ext-$sz" "$src" -c "$fam" -e "$enc" -b 100
      cli_ok "$name" || continue
      expect_eq "$name: codec" "$(probe_v "$CLI_OUT" codec_name)" "$fam"
      expect_eq "$name: container size" "$(probe_v "$CLI_OUT" width)x$(probe_v "$CLI_OUT" height)" "$want"
      expect_eq "$name: decoded size" "$(frame_v "$CLI_OUT" width)x$(frame_v "$CLI_OUT" height)" "$want"
      verdict "$name: duration is the source's ($SRC_SECONDS s)" "$(duration_ok "$CLI_OUT"; echo $?)" "$("$FFPROBE" -v error -show_entries format=duration -of default=nw=1:nk=1 "$CLI_OUT" | head -n 1) s"
    done
  done
done
