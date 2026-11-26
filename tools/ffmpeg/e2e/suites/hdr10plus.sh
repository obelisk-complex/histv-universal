#!/usr/bin/env bash
# Suite: HDR10+ (SMPTE 2094-40) routes. Sourced by run.sh.
#
# The source is made with libx265 and a JSON of per-frame metadata (dhdr10-info),
# B-frames on, so display order differs from decode order; every frame carries a
# different average_maxrgb. The metadata of each output frame is read back with
# ffprobe and compared with the source, in display order:
#   libx265 route    x265 writes the metadata itself (dhdr10-info)
#   hardware route   hevc_vaapi / hevc_vulkan cannot; the app extracts the
#                    metadata, encodes without B-frames and injects it again
#   copy route       a source under the bitrate target is stream-copied
#   SDR route        tone-mapped to SDR, the metadata must be gone
# A build whose x265 lacks HDR10+ (the system ffmpeg 6.1.1 here) cannot make the
# source: pass --gen-ffmpeg with a pinned build to test the routes that do not
# need x265's HDR10+. hdr10plus_tool is not used.

FR=24
SRC="$WORK/src/hdr10plus.mkv"
gen_hdr10plus 640 360 $FR "$SRC"
case $? in
  0) ;;
  3)
    skip "hdr10plus source" "$GEN_FFMPEG: libx265 has no HDR10+ support (dhdr10-info is ignored); pass --gen-ffmpeg with a pinned build"
    return 0
    ;;
  *)
    skip "hdr10plus source" "could not encode the source with $GEN_FFMPEG"
    return 0
    ;;
esac

want=$(hdr10plus_frames "$SRC")
expect_eq "source: $FR frames carry metadata, in display order" "$(printf '%s\n' "$want" | awk '$1 != "none"' | wc -l)" "$FR"

# hdr10plus_check <name>: the output's metadata equals the source's.
hdr10plus_check() {
  local got
  got=$(hdr10plus_frames "$CLI_OUT")
  if [ "$got" = "$want" ]; then
    verdict "$1: metadata matches the source per frame" 0 "$FR frames"
  else
    verdict "$1: metadata matches the source per frame" 1 "got: $(printf '%s' "$got" | tr '\n' ' ' | cut -c1-120)"
  fi
}

# libx265 route: needs x265 HDR10+ in the ffmpeg under test.
if caps_yes "x265 HDR10+ (dhdr10-info)" && enc_usable libx265; then
  run_cli h10p-x265 "$SRC" -c hevc -e libx265 -b 1 --hdr
  if cli_ok "libx265 route"; then
    expect_eq "libx265 route: pix_fmt" "$(probe_v "$CLI_OUT" pix_fmt)" yuv420p10le
    hdr10plus_check "libx265 route"
  fi
else
  skip "libx265 route" "the ffmpeg under test has no x265 HDR10+ (the app reports this and drops the metadata)"
  # What the app promises instead: the encode still succeeds, without metadata.
  run_cli h10p-x265-nometa "$SRC" -c hevc -e libx265 -b 1 --hdr
  if cli_ok "libx265 without HDR10+ support"; then
    expect_eq "libx265 without HDR10+ support: output has no HDR10+" "$(hdr10plus_frames "$CLI_OUT" | awk '$1 != "none"' | wc -l)" 0
  fi
fi

# Hardware route.
for enc in hevc_vaapi hevc_vulkan; do
  if ! enc_usable "$enc"; then
    skip "$enc route" "$(why_not "$enc")"
    continue
  fi
  run_cli "h10p-$enc" "$SRC" -c hevc -e "$enc" -b 1 --hdr
  if cli_ok "$enc route"; then
    expect_eq "$enc route: pix_fmt" "$(probe_v "$CLI_OUT" pix_fmt)" yuv420p10le
    hdr10plus_check "$enc route"
  fi
done

# Copy route: the source bitrate is below a 100 Mbps target.
run_cli h10p-copy "$SRC" -c hevc -b 100 --hdr
if cli_ok "copy route"; then
  hdr10plus_check "copy route"
fi

# SDR route: tone-mapped, so no HDR10+ may remain.
run_cli h10p-sdr "$SRC" -c hevc -e libx265 -b 100 --no-hdr
if cli_ok "SDR route"; then
  expect_eq "SDR route: pix_fmt" "$(probe_v "$CLI_OUT" pix_fmt)" yuv420p
  expect_eq "SDR route: no HDR10+ left" "$(hdr10plus_frames "$CLI_OUT" | awk '$1 != "none"' | wc -l)" 0
fi
