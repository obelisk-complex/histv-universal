#!/usr/bin/env bash
# Suite: encoder matrix. Sourced by run.sh.
#
# Every encoder the app can pick, in each rate-control arm it uses:
#   vbr  source above the 1 Mbps target                      -> -b:v / -maxrate
#   cqp  PQ source tone-mapped to SDR (--no-hdr), which the app always
#        re-encodes at constant quality, even below the target bitrate
#   crf  the same with --rc crf (software encoders only; hardware falls back to CQP)
# Checked: the encode succeeds and decodes; the codec is the one asked for; the
# rate-control flag the app built is on the ffmpeg command line; the flag has an
# effect (two quality values give two different file sizes: an AVOption that is
# ignored is only a warning); VBR lands near the target.
# Frame size and colour are in size-colour.sh.

W=640
H=360
FRAMES=48

file_size() { stat -c %s "$1"; }

# VBR sources are 10 s: x264 ignores -maxrate without -bufsize, and on a 2 s
# noisy clip its average-bitrate mode has not settled (4.4 Mbps against a 1 Mbps
# target at 2 s, 1.2 Mbps at 10 s).
VBR_FRAMES=240
VBR_SECONDS=10
src_sdr_h264="$WORK/src/enc-sdr-h264.mkv"
src_sdr_av1="$WORK/src/enc-sdr-av1.mkv"
src_pq_hevc="$WORK/src/enc-pq-hevc.mkv"
src_pq_av1="$WORK/src/enc-pq-av1.mkv"
gen_sdr $W $H $VBR_FRAMES h264 "$src_sdr_h264" || skip "sources" "cannot make the H.264 source with $GEN_FFMPEG"
gen_sdr $W $H $VBR_FRAMES av1 "$src_sdr_av1" || skip "sources" "cannot make the AV1 source with $GEN_FFMPEG"
gen_pq $W $H $FRAMES hevc "$src_pq_hevc" || skip "sources" "cannot make the HEVC PQ source with $GEN_FFMPEG"
gen_pq $W $H $FRAMES av1 "$src_pq_av1" || skip "sources" "cannot make the AV1 PQ source with $GEN_FFMPEG"

# The flag a constant-quality encode must carry for <encoder> at app QP <qp>.
cqp_marker() {
  case "$1" in
    av1_vaapi | av1_vulkan | av1_d3d12va)
      if [ "$1" = av1_vulkan ]; then echo "-qp $(($2 * 5))"; else echo "-global_quality $(($2 * 5))"; fi ;;
    *) echo "-qp $2" ;;
  esac
}

# matrix_case <family> <encoder> <rc>
matrix_case() {
  local fam=$1 enc=$2 rc=$3 tag name src fam_args size_lo size_hi
  tag="$enc-$rc"
  name="$enc $rc"
  fam_args="-c $fam"
  case "$fam" in
    av1) fam_args="--preserve-av1" ;;
  esac
  case "$rc" in
    vbr)
      src=$src_sdr_h264
      [ "$fam" = av1 ] && src=$src_sdr_av1
      # shellcheck disable=SC2086
      run_cli "$tag" "$src" $fam_args -e "$enc" -b 1
      cli_ok "$name" || return
      expect_eq "$name: codec" "$(probe_v "$CLI_OUT" codec_name)" "$fam"
      expect_eq "$name: container size" "$(probe_v "$CLI_OUT" width)x$(probe_v "$CLI_OUT" height)" "${W}x${H}" "$(size_defect "$enc" container)"
      case "$CLI_CMD" in *"-c:v $enc "*"-b:v 1000000"*) verdict "$name: flags" 0 "-b:v 1000000" ;; *) verdict "$name: flags" 1 "no '-c:v $enc ... -b:v 1000000' in: $CLI_CMD" ;; esac
      # Target 1 Mbps, peak 1.5. The bound is loose, but it fails for a rate
      # control that is not applied (the source is 8 Mbps).
      local kbps
      kbps=$(( $(file_size "$CLI_OUT") * 8 / VBR_SECONDS / 1000 ))
      verdict "$name: bitrate" "$([ "$kbps" -gt 100 ] && [ "$kbps" -le 2500 ]; echo $?)" "about ${kbps} kbit/s for a 1000 kbit/s target"
      ;;
    cqp | crf)
      src=$src_pq_hevc
      [ "$fam" = av1 ] && src=$src_pq_av1
      local rcflag="" lo=18 hi=42 marker_lo marker_hi
      if [ "$rc" = crf ]; then rcflag="--rc crf"; fi
      for q in $lo $hi; do
        if [ "$rc" = crf ]; then
          # shellcheck disable=SC2086
          run_cli "$tag-$q" "$src" $fam_args -e "$enc" -b 100 --no-hdr $rcflag --crf "$q"
        else
          # shellcheck disable=SC2086
          run_cli "$tag-$q" "$src" $fam_args -e "$enc" -b 100 --no-hdr --qp-i "$q" --qp-p "$q"
        fi
        cli_ok "$name q=$q" || return
        if [ "$q" = $lo ]; then size_lo=$(file_size "$CLI_OUT"); marker_lo=$CLI_CMD; else size_hi=$(file_size "$CLI_OUT"); marker_hi=$CLI_CMD; fi
      done
      expect_eq "$name: codec" "$(probe_v "$CLI_OUT" codec_name)" "$fam"
      expect_eq "$name: pix_fmt (tone-mapped to SDR)" "$(probe_v "$CLI_OUT" pix_fmt)" "yuv420p"
      if [ "$rc" = crf ]; then
        want_lo="-crf $lo"; want_hi="-crf $hi"
      else
        want_lo=$(cqp_marker "$enc" $lo); want_hi=$(cqp_marker "$enc" $hi)
      fi
      case "$marker_lo" in *" $want_lo "*) verdict "$name: flag at q=$lo" 0 "$want_lo" ;; *) verdict "$name: flag at q=$lo" 1 "no '$want_lo' in: $marker_lo" ;; esac
      case "$marker_hi" in *" $want_hi "*) verdict "$name: flag at q=$hi" 0 "$want_hi" ;; *) verdict "$name: flag at q=$hi" 1 "no '$want_hi' in: $marker_hi" ;; esac
      verdict "$name: quality flag has an effect" "$([ "$size_hi" -lt "$size_lo" ]; echo $?)" "q=$lo: $size_lo bytes, q=$hi: $size_hi bytes"
      ;;
  esac
}

# family  encoders (software first)
for spec in \
  "h264 libx264 h264_vaapi h264_vulkan h264_qsv h264_nvenc h264_amf h264_d3d12va" \
  "hevc libx265 hevc_vaapi hevc_vulkan hevc_qsv hevc_nvenc hevc_amf hevc_d3d12va" \
  "av1 libsvtav1 av1_vaapi av1_vulkan av1_qsv av1_nvenc av1_amf av1_d3d12va"; do
  set -- $spec
  fam=$1
  shift
  for enc in "$@"; do
    if ! enc_usable "$enc"; then
      skip "$enc" "$(why_not "$enc")"
      continue
    fi
    matrix_case "$fam" "$enc" vbr
    matrix_case "$fam" "$enc" cqp
    case "$enc" in lib*) matrix_case "$fam" "$enc" crf ;; esac
  done
done
