# ffmpeg end-to-end harness

Runs the app's real ffmpeg argument builder against a given ffmpeg/ffprobe pair
and checks the outputs with ffprobe. It drives `histv-cli`, so the arguments
are the ones the app builds, not a copy of them. Sources are synthetic and made
with ffmpeg alone.

`../verify.sh` checks a built archive in CI (components compiled in, linked
libraries, tiny real jobs). This harness is the other half: it needs the CLI
and, for the hardware encoders, a GPU, so it is run by hand and is not wired
into any workflow.

## Run

```sh
cd src-tauri && cargo build --no-default-features --features cli,dovi --bin histv-cli
cd ..
tools/ffmpeg/e2e/run.sh --ffmpeg /path/to/ffmpeg            # ffprobe is taken from beside it
tools/ffmpeg/e2e/run.sh --ffmpeg ... --ffprobe ... --suite encoders --suite animated
```

| Option | Environment | Default |
|---|---|---|
| `--ffmpeg PATH` | `FFMPEG` | `ffmpeg` on `PATH` |
| `--ffprobe PATH` | `FFPROBE` | beside the ffmpeg |
| `--cli PATH` | `HISTV_CLI` | `src-tauri/target/debug/histv-cli` |
| `--gen-ffmpeg PATH` | `GEN_FFMPEG` | the ffmpeg under test |
| `--suite NAME` (repeatable) | `E2E_SUITES` | all |
| `--keep` | | keep the scratch directory and print its path |

`--gen-ffmpeg` names the ffmpeg that makes the source files. Use it when the
build under test cannot make a source (the system ffmpeg 6.1.1 has no HDR10+
in x265; the pinned build has no WebP encoder, so animated WebP sources fall
back to any ffmpeg on `PATH` that has `libwebp_anim`).

To get the pinned build: download `histv-ffmpeg-linux-x64.tar.gz` for the tag in
`src-tauri/src/ffmpeg.rs` (`histv_release`) from the repository's releases,
check its sha256 against the same file, and extract it into a scratch directory.

Everything runs in one `mktemp -d` directory that is removed on exit.
`histv-cli` runs from a copy of itself with `ffmpeg` and `ffprobe` symlinked
beside it (the app looks beside its own executable first), with `HOME` and
`XDG_*` pointing into the scratch directory and no `DISPLAY`. Nothing is read
from or written to the user's configuration. Generated media is never kept.

Exit status is 0 when nothing FAILed, 1 otherwise, 2 for a usage error.

## Results

| Status | Meaning |
|---|---|
| PASS | the assertion held |
| FAIL | it did not, and no documented defect explains it |
| XFAIL | it did not, and a documented ffmpeg or driver defect explains it |
| XPASS | an assertion marked as a known defect held; the defect may be fixed |
| SKIP | not run: device, tool or ffmpeg capability absent (the reason is printed) |

An XFAIL is only ever declared in `lib.sh` (`size_defect`) with its reason.
An unexplained failure stays a FAIL.

## Suites

| Suite | Covers |
|---|---|
| `encoders` | every encoder the app can pick, per codec (H.264, HEVC, AV1) and rate-control arm: VBR; constant quality (CQP); CRF for software encoders. Checks the encode decodes, the codec, the rate-control flag on the command line, that the quality flag changes the output (two values, two sizes; an ignored AVOption is only a warning), and that VBR lands near the target |
| `size-colour` | picture size at 640x360, 1920x1080 and 1000x562, in the container and as decoded; colour primaries, transfer and matrix (BT.709 SDR, BT.2020 PQ HDR) in the container and in the bitstream; 10-bit output for HDR |
| `animated` | GIF and animated WebP (through the app's own compositor) at an even and an odd size, with software and hardware encoders; APNG once, at the even size, with libx265: codec, even-padded size, duration |
| `hdr10plus` | HDR10+ per-frame metadata compared with the source in display order, with B-frames in the source: the libx265 route, the hardware route (extract, encode without B-frames, inject; static HDR10 only for `hevc_vaapi` on ffmpeg older than 7), the copy route, and the SDR route (metadata must be gone) |

Known defects declared XFAIL:

- `av1_vulkan` (n8.1.3): the aligned surface size is written as the AV1 frame
  size, so the container and the decoder report 640x368, 1920x1088, 1024x576.
- `av1_vaapi` on AMD VCN (Mesa radeonsi): the stream decodes taller than asked
  (640x362, 1920x1082, 1024x576); the container is right. Only declared when
  the lowest render node is an AMD GPU.

### Not rebuilt

- **Dolby Vision (profiles 5, 7, 8.x, RPU order).** Sources need RPUs written
  by `dovi_tool`, which is not installed on the host this was rebuilt on, and
  the description of the original assertions is too thin to rebuild them without
  being able to run them. `verify.sh` already asserts that libx265 and libsvtav1
  have the `-dolbyvision` option.
- `hdr10plus_tool` is not needed: HDR10+ sources are written by libx265
  (`dhdr10-info`) and read back with ffprobe.

## Hardware

Hardware encoders run only when the app's own encoder detection accepts them
(a one-frame test encode), so the harness gates on exactly what the app decided.
They run headless through `/dev/dri`, with no display.

| Encoder | Needs | Where it ran |
|---|---|---|
| `*_vaapi` | a DRM render node and a VAAPI driver | AMD (Mesa radeonsi) |
| `*_vulkan` | a Vulkan video-encode driver (RADV); `av1_vulkan` is not auto-selected by the app, so the harness probes it with the app's device set-up | AMD |
| `*_qsv` | an Intel GPU | SKIP (none) |
| `*_nvenc` | an NVIDIA GPU | SKIP (none) |
| `*_amf` | AMD proprietary stack (Windows, or amdgpu-pro) | SKIP (none) |
| `*_d3d12va` | Windows with a D3D12 video-encode device | SKIP (Linux only here) |

The Vulkan encoders do not exist in the system ffmpeg 6.1.1, so they SKIP there.
AMD VCN refuses pictures below 130x128, which is why the animated suite starts
at 320x180.

## Last run

2026-11-26, Linux x86-64, AMD GPUs with Mesa radeonsi, headless.

| ffmpeg | PASS | FAIL | XFAIL | XPASS | SKIP |
|---|---|---|---|---|---|
| n8.1.3, `ffmpeg-n8.1.3-histv.2` (linux-x64 archive, sha256 checked) | 295 | 0 | 10 | 0 | 12 |
| 6.1.1-3ubuntu5 (`/usr/bin/ffmpeg`), sources made by the n8.1.3 build | 208 | 0 | 3 | 0 | 22 |

The 10 XFAIL on n8.1.3: `av1_vulkan` container and decoded size at three sizes
(6), once more in the encoder matrix (1), and `av1_vaapi` decoded size at three
sizes (3). On 6.1.1 only the `av1_vaapi` three remain. The 12 and 22 SKIP are
the encoders listed above as absent, plus the Vulkan encoders and the libx265
HDR10+ route on 6.1.1. No XPASS: both known defects are still present.

Nothing failed unexplained. The APNG probe failure of the previous run
(`histv-cli` could not read a duration from an APNG written by ffmpeg, so the
`animated` suite failed on both builds) is fixed: the probe reads the largest
packet pts + duration as a last tier, and the APNG case passes on both builds.

On 6.1.1 the `hevc_vaapi` route of `hdr10plus` passes with the outcome the app
gives there: static HDR10, not HDR10+. AMD (Mesa) `hevc_vaapi` writes a
different PPS in the packets than in the Matroska CodecPrivate. The app's demux
step (`-bsf:v hevc_mp4toannexb`) adds the container's parameter sets to the
stream. Before ffmpeg 7 it puts them after the in-band ones, so the decoder
takes the stale PPS and reports `cu_qp_delta` errors (5 in 24 frames); n8.1.3
puts them before, and the stream decodes cleanly. The injection step is not
involved. The app keeps static HDR10 only for `hevc_vaapi` on an ffmpeg older
than 7, and the suite asserts that there: clean decode, PQ transfer, no HDR10+
metadata. On n8.1.3 the same route keeps the per-frame HDR10+ metadata (24
frames match the source).
