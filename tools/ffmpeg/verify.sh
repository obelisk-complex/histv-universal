#!/usr/bin/env bash
# Conformance check for a histv ffmpeg/ffprobe pair.
#
# Usage: verify.sh <dir-containing-ffmpeg-and-ffprobe> <linux|windows|macos>
#
# Checks that every component histv invokes is compiled in, that the platform
# HW encoders are listed (not run: CI has no GPU), that the binary links only
# system libraries, and runs tiny real jobs in the argument shapes histv uses
# (src-tauri/src/encoder.rs, hevc_utils.rs, webp_decode.rs, mkv_tags.rs,
# dovi_pipeline.rs, hdr10plus_pipeline.rs, probe.rs).
#
# Exits 0 when everything passes. Otherwise prints one "FAIL: ..." line per
# missing item and a summary line, and exits 1.
#
# Portability: must run under macOS bash 3.2 and MSYS2 bash. Native Windows
# ffmpeg cannot read MSYS /dev/* or /tmp paths, so every job runs inside a
# temporary working directory with relative file names only.

set -u

usage() {
  echo "usage: $0 <dir-containing-ffmpeg-and-ffprobe> <linux|windows|macos>" >&2
  exit 2
}

[ $# -eq 2 ] || usage
PLATFORM=$2
case "$PLATFORM" in
  linux | macos) EXE="" NULLDEV=/dev/null ;;
  windows) EXE=".exe" NULLDEV=NUL ;;
  *) usage ;;
esac

BIN_DIR=$(cd "$1" 2>/dev/null && pwd) || { echo "FAIL: no such directory: $1" >&2; exit 1; }
FF="$BIN_DIR/ffmpeg$EXE"
FP="$BIN_DIR/ffprobe$EXE"
for b in "$FF" "$FP"; do
  [ -x "$b" ] || { echo "FAIL: missing or not executable: $b" >&2; exit 1; }
done

WORK=$(mktemp -d "${TMPDIR:-/tmp}/histv-verify.XXXXXX") || { echo "FAIL: mktemp" >&2; exit 1; }
trap 'rm -rf "$WORK"' EXIT
cd "$WORK" || exit 1

NPASS=0
NFAIL=0
FAILED=""

ok() { NPASS=$((NPASS + 1)); }
bad() {
  NFAIL=$((NFAIL + 1))
  FAILED="$FAILED${FAILED:+, }$1"
  echo "FAIL: $1" >&2
}

# --- 1. The binaries start -------------------------------------------------

if "$FF" -hide_banner -version >version.txt 2>&1; then
  ok
  tr -d '\r' <version.txt | sed -n '1p;/^configuration:/p'
else
  bad "ffmpeg -version does not run"
  tr -d '\r' <version.txt | tail -n 5 >&2
  echo "verify.sh: FAIL (binary does not start)" >&2
  exit 1
fi
if "$FP" -hide_banner -version >/dev/null 2>&1; then ok; else bad "ffprobe -version does not run"; fi

# --- 2. Compiled-in components ---------------------------------------------

# Write one component name per line for a listing kind into list_<kind>.txt.
list_names() {
  "$FF" -hide_banner "-$1" 2>/dev/null | tr -d '\r' >"raw_$1.txt"
  case "$1" in
    encoders | decoders | pix_fmts)
      # " V....D libx264   ..." after a " ------" separator.
      awk 'f && NF >= 2 { print $2 } /^ *-+$/ { f = 1 }' "raw_$1.txt" ;;
    muxers | demuxers)
      # " DEd name[,alias] ..." after " ---"; flags are fixed width, may hold spaces.
      awk 'f { n = split(substr($0, 6), w, " "); if (n) { k = split(w[1], a, ","); for (i = 1; i <= k; i++) print a[i] } }
           /^ *-+$/ { f = 1 }' "raw_$1.txt" ;;
    filters)
      # " TSC zscale  V->V  ..."
      awk '$3 ~ /->/ { print $2 }' "raw_$1.txt" ;;
    bsfs)
      awk 'NR > 1 && NF { print $1 }' "raw_$1.txt" ;;
    protocols)
      awk '/^ +[a-z]/ { print $1 }' "raw_$1.txt" ;;
  esac >"list_$1.txt"
}

# need <kind> <name>... : each name must appear in list_<kind>.txt.
need() {
  kind=$1
  shift
  label=$(echo "$kind" | sed 's/s$//')
  for n in "$@"; do
    if grep -qx "$n" "list_$kind.txt"; then ok; else bad "missing $label: $n"; fi
  done
}

for k in encoders decoders muxers demuxers filters bsfs protocols pix_fmts; do
  list_names "$k"
  [ -s "list_$k.txt" ] || bad "could not list $k"
done

# Software video encoders (encoder.rs priority tables) and everything the
# audio/subtitle/raw paths emit. "mp3" maps to libmp3lame at run time.
need encoders libx264 libx265 libsvtav1 \
  aac ac3 eac3 libopus libvorbis libmp3lame flac mp2 \
  pcm_s16le pcm_s24le pcm_s32le pcm_f32le pcm_s16be pcm_alaw pcm_mulaw \
  mov_text rawvideo gif apng
# Decoders for histv's accepted inputs (queue.rs extensions). libdav1d is the
# only software AV1 decoder: ffmpeg's native "av1" decoder is hwaccel-only.
need decoders h264 hevc libdav1d vp9 vp8 mpeg2video mpeg1video mpeg4 msmpeg4 \
  vc1 wmv3 rv40 prores dnxhd mjpeg gif apng webp png ffv1 rawvideo theora \
  aac ac3 eac3 mp3 mp2 alac flac opus vorbis truehd dca wmav2 cook \
  pcm_s16le pcm_s24le \
  subrip ass ssa mov_text pgssub dvdsub
need demuxers matroska mov avi mpegts asf flv ogg rm mpeg gif apng webp_pipe \
  image2 yuv4mpegpipe rawvideo hevc h264 lavfi aac ac3 mp3 wav srt ass
need muxers mp4 mov matroska hevc rawvideo null adts
need filters zscale tonemap format hwupload pad color scale aresample setparams
need bsfs hevc_mp4toannexb hevc_metadata h264_mp4toannexb aac_adtstoasc
need protocols file pipe
need pix_fmts yuv420p yuv420p10le p010le nv12 rgba rgb24

# Vulkan: the hwcontext must be compiled in (it is listed as a device type).
# The loader is dlopened, so on a host without libvulkan the device open then
# reports "Unable to open the libvulkan library" while everything else still
# works; that state is printed, not failed. No GPU or loader needed.
check_vulkan_hwcontext() {
  if "$FF" -hide_banner -init_hw_device list 2>/dev/null | tr -d '\r' | grep -qx 'vulkan'; then
    ok
  else
    bad "Vulkan hwcontext not compiled in"
    return
  fi
  if "$FF" -hide_banner -v verbose -init_hw_device vulkan=vk \
    -f lavfi -i nullsrc -frames:v 1 -f null - >vulkan.log 2>&1; then
    echo "vulkan: device created on this host"
  elif grep -q 'Unable to open the libvulkan library' vulkan.log; then
    echo "vulkan: compiled in, no loader on this host"
  else
    echo "vulkan: compiled in, no usable device on this host"
  fi
}

# Native Dolby Vision RPU coding is an encoder option (needs a DV source to
# exercise end to end), so assert the option exists.
for enc in libx265 libsvtav1; do
  if "$FF" -hide_banner -h "encoder=$enc" 2>/dev/null | tr -d '\r' | grep -q '^ *-dolbyvision '; then
    ok
  else
    bad "$enc has no -dolbyvision option"
  fi
done

# Platform HW encoders: listed only (compile-time proof; no GPU in CI).
case "$PLATFORM" in
  linux)
    need encoders h264_nvenc hevc_nvenc av1_nvenc h264_vaapi hevc_vaapi av1_vaapi \
      h264_amf hevc_amf av1_amf h264_qsv hevc_qsv av1_qsv \
      h264_vulkan hevc_vulkan av1_vulkan
    need filters scale_vaapi
    check_vulkan_hwcontext
    # VAAPI must reach a DRM render node. Without the DRM display path
    # (vaapi_drm) the encoders still list but every device open fails with
    # "No VA display found". A missing node path tells the two apart, no GPU
    # needed: only the DRM branch tries to open it as a DRM device node.
    "$FF" -hide_banner -v verbose -init_hw_device vaapi=va:histv-no-such-render-node \
      -f lavfi -i nullsrc -frames:v 1 -f null - >vaapi_drm.log 2>&1
    if grep -q 'as DRM device node' vaapi_drm.log; then ok; else bad "VAAPI built without the DRM display path (vaapi_drm)"; fi ;;
  windows)
    need encoders h264_nvenc hevc_nvenc av1_nvenc h264_amf hevc_amf av1_amf \
      h264_qsv hevc_qsv av1_qsv h264_vulkan hevc_vulkan av1_vulkan \
      h264_d3d12va hevc_d3d12va av1_d3d12va
    check_vulkan_hwcontext ;;
  macos)
    need encoders h264_videotoolbox hevc_videotoolbox ;;
esac

# Exact feature switches. Static libraries leave no trace in the DLL/dylib/ELF
# imports, so the configure line is what shows what was linked in: with
# --disable-autodetect (and --disable-iconv, which autodetect does not cover)
# only the --enable-* named here can be. Same external libraries everywhere;
# only the hardware APIs differ by platform.
want_config="--disable-autodetect --disable-iconv --enable-gpl --enable-version3
  --enable-static --enable-ffprobe --enable-indev=lavfi --enable-runtime-cpudetect
  --enable-zlib --enable-libx264 --enable-libx265 --enable-libsvtav1
  --enable-libdav1d --enable-libzimg --enable-libopus --enable-libvorbis
  --enable-libmp3lame"
case "$PLATFORM" in
  linux) want_config="$want_config --enable-vaapi --enable-libdrm --enable-ffnvcodec
    --enable-cuda --enable-cuvid --enable-nvdec --enable-nvenc --enable-amf
    --enable-libvpl --enable-vulkan" ;;
  windows) want_config="$want_config --enable-ffnvcodec --enable-cuda --enable-cuvid
    --enable-nvdec --enable-nvenc --enable-d3d11va --enable-dxva2 --enable-d3d12va
    --enable-amf --enable-libvpl --enable-vulkan" ;;
  macos) want_config="$want_config --enable-videotoolbox --enable-audiotoolbox" ;;
esac
"$FF" -hide_banner -version | tr -d '\r' | sed -n 's/^configuration: //p' | tr ' ' '\n' |
  tr -d "'\"" | grep -E '^--enable-|^--disable-(autodetect|iconv)$' | LC_ALL=C sort -u >config_have.txt
printf '%s\n' $want_config | LC_ALL=C sort -u >config_want.txt
if cmp -s config_have.txt config_want.txt; then
  ok
else
  extra=$(LC_ALL=C comm -23 config_have.txt config_want.txt | tr '\n' ' ')
  lack=$(LC_ALL=C comm -13 config_have.txt config_want.txt | tr '\n' ' ')
  bad "configure switches differ (unexpected: ${extra:-none}; missing: ${lack:-none})"
fi

# --- 3. Links only system libraries ------------------------------------------

check_deps() {
  bin=$1
  case "$PLATFORM" in
    linux)
      command -v readelf >/dev/null 2>&1 || { bad "readelf not found, cannot check $bin"; return; }
      readelf -d "$bin" | sed -n 's/.*Shared library: \[\(.*\)\].*/\1/p' >deps.txt
      extra=$(grep -vxE 'libc\.so\.6|libm\.so\.6|libpthread\.so\.0|libdl\.so\.2|librt\.so\.1|ld-linux-x86-64\.so\.2|libmvec\.so\.1' deps.txt | tr '\n' ' ')
      ;;
    macos)
      otool -L "$bin" | tail -n +2 | awk '{ print $1 }' >deps.txt
      extra=$(grep -vE '^(/usr/lib/|/System/Library/)' deps.txt | tr '\n' ' ')
      ;;
    windows)
      command -v objdump >/dev/null 2>&1 || { bad "objdump not found, cannot check $bin"; return; }
      objdump -p "$bin" | tr -d '\r' | sed -n 's/.*DLL Name: *//p' >deps.txt
      # Anything from the MinGW/MSYS runtime or a third-party library would
      # have to ship next to the exe; system DLLs do not look like this.
      extra=$(grep -iE '^(lib.*|msys-.*|cyg.*|zlib.*|.*pthread.*|.*stdc\+\+.*|.*gcc.*)\.dll$' deps.txt | tr '\n' ' ')
      ;;
  esac
  echo "$(basename "$bin") links: $(tr '\n' ' ' <deps.txt)"
  if [ -n "$extra" ]; then bad "$(basename "$bin") links non-system libraries: $extra"; else ok; fi
}
check_deps "$FF"
check_deps "$FP"
if [ "$PLATFORM" = linux ] && command -v objdump >/dev/null 2>&1; then
  echo "highest glibc symbol: $(objdump -T "$FF" | grep -o 'GLIBC_[0-9.]*' | sort -t. -k1,1 -k2,2n -k3,3n | tail -n 1)"
fi
if [ "$PLATFORM" = macos ]; then
  echo "minimum macOS: $(otool -l "$FF" | awk '/LC_BUILD_VERSION/ { f = 1 } f && $1 == "minos" { print $2; exit }')"
fi

# --- 4. Real runs in histv's argument shapes --------------------------------

# run <label> <ffmpeg|ffprobe args...>: the command must exit 0.
run() {
  label=$1
  shift
  if "$@" >"log.txt" 2>&1; then
    ok
    return 0
  fi
  bad "$label"
  tr -d '\r' <log.txt | grep -v '^ *$' | tail -n 4 | sed 's/^/    /' >&2
  return 1
}

# probe_is <label> <file> <stream> <entry> <expected>
probe_is() {
  got=$("$FP" -v error -select_streams "$3" -show_entries "stream=$4" \
    -of default=nw=1:nk=1 "$2" 2>/dev/null | tr -d '\r' | head -n 1)
  if [ "$got" = "$5" ]; then ok; else bad "$1: $3 $4 is '$got', expected '$5'"; fi
}

Q="-hide_banner -nostdin -v error -y"
SRC_V="testsrc2=s=256x144:r=10:d=1"
SRC_A="sine=frequency=440:sample_rate=48000:duration=1"
TONEMAP="zscale=t=linear,tonemap=hable:desat=0,zscale=t=bt709:p=bt709:m=bt709:r=tv,format=yuv420p"

# Encoder detection probe (encoder.rs test_encoder), software path.
run "lavfi colour source + nv12 detection encode" \
  "$FF" $Q -f lavfi -i "color=black:s=256x256:d=0.04:r=25" -frames:v 1 -pix_fmt nv12 -c:v libx264 detect.mp4

# Video encoders.
run "libx264 encode" "$FF" $Q -f lavfi -i "$SRC_V" -c:v libx264 -preset ultrafast -pix_fmt yuv420p h264.mp4 &&
  probe_is "libx264 encode" h264.mp4 v:0 codec_name h264
run "libx265 8-bit encode" "$FF" $Q -f lavfi -i "$SRC_V" -c:v libx265 -preset ultrafast -pix_fmt yuv420p hevc8.mkv &&
  probe_is "libx265 8-bit encode" hevc8.mkv v:0 pix_fmt yuv420p
run "libx265 10-bit PQ encode" "$FF" $Q -f lavfi \
  -i "$SRC_V,format=yuv420p10le,setparams=color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc:range=tv" \
  -c:v libx265 -preset ultrafast -pix_fmt yuv420p10le \
  -color_primaries bt2020 -color_trc smpte2084 -colorspace bt2020nc -color_range tv hevc10.mkv &&
  probe_is "libx265 10-bit PQ encode" hevc10.mkv v:0 color_transfer smpte2084
run "libsvtav1 encode" "$FF" $Q -f lavfi -i "$SRC_V" -c:v libsvtav1 -preset 12 -crf 40 -pix_fmt yuv420p av1.mkv &&
  probe_is "libsvtav1 encode" av1.mkv v:0 codec_name av1
# No decoder forced, as in histv: the default AV1 decoder must work without a GPU.
run "AV1 software decode" "$FF" $Q -i av1.mkv -f null -

# HDR->SDR tonemap chain (encoder.rs TONEMAP) on the 10-bit PQ file.
run "zscale/tonemap chain" "$FF" $Q -i hevc10.mkv -vf "$TONEMAP" -c:v libx264 -preset ultrafast tonemap.mkv &&
  probe_is "zscale/tonemap chain" tonemap.mkv v:0 pix_fmt yuv420p

# MP4 output: faststart + hvc1 tag (encoder.rs container args).
run "mp4 +faststart -tag:v hvc1" "$FF" $Q -i hevc8.mkv -c:v copy -movflags +faststart -tag:v hvc1 hvc1.mp4 &&
  probe_is "mp4 +faststart -tag:v hvc1" hvc1.mp4 v:0 codec_tag_string hvc1

# Raw Annex B demux (hevc_utils.rs) and the hevc_metadata VAAPI crop fix.
run "hevc_mp4toannexb to raw hevc" "$FF" $Q -i hvc1.mp4 -c:v copy -bsf:v hevc_mp4toannexb -an -sn -f hevc annexb.hevc
run "hevc_metadata crop" "$FF" $Q -i hevc8.mkv -c:v copy -bsf:v hevc_metadata=crop_bottom=8 crop.mkv &&
  probe_is "hevc_metadata crop" crop.mkv v:0 height 136

# Audio encoders as histv emits them (encoder.rs build_audio_args_from_probe).
run "aac encode" "$FF" $Q -f lavfi -i "$SRC_A" -ac 2 -c:a:0 aac -b:a:0 640k aac.mp4
run "ac3 encode" "$FF" $Q -f lavfi -i "$SRC_A" -ac 2 -c:a:0 ac3 -b:a:0 640k ac3.mkv
run "eac3 encode" "$FF" $Q -f lavfi -i "$SRC_A" -ac 2 -c:a:0 eac3 -b:a:0 640k eac3.mp4
run "libopus encode" "$FF" $Q -f lavfi -i "$SRC_A" -ac 2 -c:a:0 libopus -b:a:0 512k opus.mkv &&
  probe_is "libopus encode" opus.mkv a:0 codec_name opus
run "libvorbis encode" "$FF" $Q -f lavfi -i "$SRC_A" -ac 2 -c:a:0 libvorbis -b:a:0 320k vorbis.mkv &&
  probe_is "libvorbis encode" vorbis.mkv a:0 codec_name vorbis
run "mp3 encode (-c:a mp3)" "$FF" $Q -f lavfi -i "$SRC_A" -ac 2 -c:a:0 mp3 -b:a:0 128k mp3.mkv &&
  probe_is "mp3 encode (-c:a mp3)" mp3.mkv a:0 codec_name mp3
run "flac encode" "$FF" $Q -f lavfi -i "$SRC_A" -ac 2 -c:a:0 flac flac.mkv

# DV pipeline audio extract to ADTS (dovi_pipeline.rs).
run "audio copy to .aac (adts)" "$FF" $Q -i aac.mp4 -vn -sn -c:a copy audio.aac

# HDR10+ remux shape (hdr10plus_pipeline.rs): raw hevc + audio/subs by copy.
# MP4 only: raw Annex B carries no timestamps and the matroska muxer rejects
# that with any ffmpeg build (an app-side issue, not a build property).
run "raw hevc remux with optional maps" "$FF" $Q -i annexb.hevc -i ac3.mkv \
  -map 0:v:0 -map "1:a?" -map "1:s?" -c copy remux.mp4

# Subtitles: text to mov_text in MP4, copy into MKV.
printf '1\r\n00:00:00,000 --> 00:00:00,800\r\nhello\r\n\r\n' >subs.srt
run "mov_text subtitles into mp4" "$FF" $Q -i h264.mp4 -i subs.srt -map 0:v -map 1:s \
  -c:v copy -c:s mov_text -disposition:s:0 default subs.mp4 &&
  probe_is "mov_text subtitles into mp4" subs.mp4 s:0 codec_name mov_text
run "subtitle copy into mkv" "$FF" $Q -i h264.mp4 -i subs.srt -map 0:v -map 1:s -c:v copy -c:s copy subs.mkv

# Image sources (gif/apng) with the even-dimension pad (encoder.rs).
run "gif encode" "$FF" $Q -f lavfi -i "testsrc2=s=63x47:r=5:d=1" gif.gif
run "apng encode" "$FF" $Q -f lavfi -i "testsrc2=s=63x47:r=5:d=1" -f apng anim.apng
run "gif decode + pad" "$FF" $Q -i gif.gif -vf "pad=ceil(iw/2)*2:ceil(ih/2)*2" -c:v libx264 -pix_fmt yuv420p gif.mp4
run "apng decode + pad" "$FF" $Q -i anim.apng -vf "pad=ceil(iw/2)*2:ceil(ih/2)*2" -c:v libx264 -pix_fmt yuv420p apng.mp4

# rawvideo RGBA over pipes (webp_decode.rs encoder side).
if (set -o pipefail
  "$FF" $Q -f lavfi -i "testsrc2=s=64x64:r=10:d=0.5" -f rawvideo -pix_fmt rgba pipe:1 2>pipe1.log |
    "$FF" $Q -f rawvideo -pix_fmt rgba -s 64x64 -r 10 -i pipe:0 -an -sn -c:v libx264 -pix_fmt yuv420p \
      -movflags +faststart pipe.mp4 2>pipe2.log); then
  ok
else
  bad "rawvideo over pipe:"
  cat pipe1.log pipe2.log | tr -d '\r' | tail -n 4 | sed 's/^/    /' >&2
fi

# x265 HDR10+ (ENABLE_HDR10_PLUS): dynamic metadata from an LLC-format JSON
# must come out as SMPTE 2094-40 side data on the decoded frames.
{
  printf '{"JSONInfo":{"HDR10plusProfile":"B","Version":"1.0"},"SceneInfo":['
  i=0
  while [ $i -lt 5 ]; do
    [ $i -gt 0 ] && printf ','
    printf '{"SceneFrameIndex":%d,"SceneId":0,"SequenceFrameIndex":%d,' $i $i
    printf '"LuminanceParameters":{"AverageRGB":120,"MaxScl":[900,800,700],'
    printf '"LuminanceDistributions":{"DistributionIndex":[1,5,10,25,50,75,90,95,99],'
    printf '"DistributionValues":[1,3,6,20,60,150,400,600,850]}},'
    printf '"NumberOfWindows":1,"TargetedSystemDisplayMaximumLuminance":400}'
    i=$((i + 1))
  done
  printf ']}'
} >hdr10plus.json
run "libx265 HDR10+ (dhdr10-info)" "$FF" $Q -i hevc10.mkv -frames:v 5 -c:v libx265 -preset ultrafast \
  -x265-params "dhdr10-info=hdr10plus.json" hdr10plus.mkv &&
  if "$FP" -v error -select_streams v:0 -read_intervals "%+#1" -show_frames \
    -show_entries frame_side_data=side_data_type hdr10plus.mkv 2>/dev/null | tr -d '\r' |
    grep -q 'SMPTE2094-40'; then
    ok
  else
    bad "libx265 HDR10+: no SMPTE 2094-40 side data in the output"
  fi

# Two-pass: pass 1 to the null device with a container format (encoder.rs).
run "two-pass pass 1 to $NULLDEV" "$FF" $Q -f lavfi -i "$SRC_V" -c:v libx264 -b:v 200k \
  -pass 1 -passlogfile twopass -an -sn -f mp4 "$NULLDEV" &&
  run "two-pass pass 2" "$FF" $Q -f lavfi -i "$SRC_V" -c:v libx264 -b:v 200k \
    -pass 2 -passlogfile twopass twopass.mp4

# Stream integrity check (mkv_tags.rs).
run "-f null - integrity check" "$FF" -v error -stats -i hevc8.mkv -map 0:v:0 -c copy -f null -

# ffprobe JSON (probe.rs).
if "$FP" -v error -show_streams -show_format -print_format json hvc1.mp4 >probe.json 2>&1 &&
  grep -q '"codec_name": "hevc"' probe.json && grep -q '"format_name"' probe.json; then
  ok
else
  bad "ffprobe -show_streams -show_format -print_format json"
fi

# --- Summary ------------------------------------------------------------------

TOTAL=$((NPASS + NFAIL))
if [ "$NFAIL" -eq 0 ]; then
  echo "verify.sh: PASS ($NPASS/$TOTAL checks, $PLATFORM)"
  exit 0
fi
echo "verify.sh: FAIL ($NFAIL of $TOTAL checks, $PLATFORM): $FAILED" >&2
exit 1
