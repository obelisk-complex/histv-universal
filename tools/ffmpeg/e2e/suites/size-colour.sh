#!/usr/bin/env bash
# Suite: frame size and colour metadata. Sourced by run.sh.
#
# Several hardware encoders write a padded or aligned size into the bitstream
# while the container keeps the requested one, so the file looks right to ffprobe
# and wrong to a decoder. Each (encoder, size) encode is checked two ways:
#   container  what ffprobe reports for the stream
#   decoded    the size of the first frame the decoder produces
# Sizes: 640x360 and 1920x1080 (the heights AMD VCN pads), and 1000x562 (neither
# dimension aligned). Software encoders run too, as the control.
#
# Colour: a PQ/BT.2020 source kept as HDR (--hdr) must come out 10-bit with the
# same primaries, transfer and matrix, in the container and in the bitstream; an
# SDR BT.709 source must stay BT.709.
#
# Known defects (XFAIL), from the wiki and encoder.rs:
#   av1_vulkan   n8.1.3 writes the aligned surface size as the frame size
#   av1_vaapi    on AMD VCN (Mesa radeonsi) the AV1 stream decodes taller than asked

SIZES="640x360 1920x1080 1000x562"
FR=24

for sz in $SIZES; do
  w=${sz%x*}
  h=${sz#*x}
  gen_sdr "$w" "$h" $FR h264 "$WORK/src/sc-sdr-$sz-h264.mkv" || skip "sources $sz" "cannot make an H.264 source with $GEN_FFMPEG"
  gen_sdr "$w" "$h" $FR av1 "$WORK/src/sc-sdr-$sz-av1.mkv" || skip "sources $sz" "cannot make an AV1 source with $GEN_FFMPEG"
done
gen_pq 640 360 $FR hevc "$WORK/src/sc-pq-hevc.mkv" || skip "sources pq" "cannot make an HEVC PQ source with $GEN_FFMPEG"
gen_pq 640 360 $FR av1 "$WORK/src/sc-pq-av1.mkv" || skip "sources pq" "cannot make an AV1 PQ source with $GEN_FFMPEG"

# colour_check <name> <file> <primaries> <transfer> <space> [xfail-reason]
colour_check() {
  local got
  got="$(probe_v "$2" color_primaries)/$(probe_v "$2" color_transfer)/$(probe_v "$2" color_space)"
  expect_eq "$1: container colour" "$got" "$3/$4/$5" "${6:-}"
  got="$(frame_v "$2" color_primaries)/$(frame_v "$2" color_transfer)/$(frame_v "$2" color_space)"
  expect_eq "$1: bitstream colour" "$got" "$3/$4/$5" "${6:-}"
}

for spec in \
  "h264 libx264 h264_vaapi h264_vulkan" \
  "hevc libx265 hevc_vaapi hevc_vulkan" \
  "av1 libsvtav1 av1_vaapi av1_vulkan"; do
  set -- $spec
  fam=$1
  shift
  for enc in "$@"; do
    if ! enc_usable "$enc"; then
      skip "$enc" "$(why_not "$enc")"
      continue
    fi
    fam_args="-c $fam"
    srcfam=h264
    if [ "$fam" = av1 ]; then fam_args="--preserve-av1"; srcfam=av1; fi

    for sz in $SIZES; do
      name="$enc $sz"
      # shellcheck disable=SC2086
      run_cli "sc-$enc-$sz" "$WORK/src/sc-sdr-$sz-$srcfam.mkv" $fam_args -e "$enc" -b 1
      cli_ok "$name" || continue
      expect_eq "$name: container size" "$(probe_v "$CLI_OUT" width)x$(probe_v "$CLI_OUT" height)" "$sz" "$(size_defect "$enc" container)"
      expect_eq "$name: decoded size" "$(frame_v "$CLI_OUT" width)x$(frame_v "$CLI_OUT" height)" "$sz" "$(size_defect "$enc" decoded)"
    done

    # SDR colour is checked at 640x360, HDR for HEVC and AV1 only (no 10-bit H.264).
    run_cli "sc-$enc-sdr-colour" "$WORK/src/sc-sdr-640x360-$srcfam.mkv" $fam_args -e "$enc" -b 1
    if cli_ok "$enc SDR"; then
      expect_eq "$enc SDR: pix_fmt" "$(probe_v "$CLI_OUT" pix_fmt)" yuv420p
      colour_check "$enc SDR" "$CLI_OUT" bt709 bt709 bt709
    fi
    [ "$fam" = h264 ] && continue
    pq=$WORK/src/sc-pq-hevc.mkv
    [ "$fam" = av1 ] && pq=$WORK/src/sc-pq-av1.mkv
    # shellcheck disable=SC2086
    run_cli "sc-$enc-hdr" "$pq" $fam_args -e "$enc" -b 1 --hdr
    if cli_ok "$enc HDR"; then
      expect_eq "$enc HDR: pix_fmt" "$(probe_v "$CLI_OUT" pix_fmt)" yuv420p10le
      colour_check "$enc HDR" "$CLI_OUT" bt2020 smpte2084 bt2020nc
    fi
  done
done
