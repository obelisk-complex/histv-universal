# dv-compare

Did the Dolby Vision data and the picture survive a transcode? One command answers it with numbers, so "is this output subtly damaged?" does not need an afternoon of hand analysis.

```
tools/dv-compare/run.sh compare INPUT OUTPUT [--mode copy|reencode] [options]
tools/dv-compare/run.sh selftest
```

Needs `ffmpeg` and `ffprobe` (from `PATH`, or `FFMPEG` / `FFPROBE`), `python3` and `cargo` (builds the small RPU helper in `rpu-dump/` on first use, offline once the crates are cached). `selftest` also needs `MP4Box`. It reads the two files and writes only to a temporary directory; it never touches a display or any config.

## Checks

Each prints `PASS`, `DIFFERENT` or `SKIP` with numbers.

| Check | What is compared |
|---|---|
| `container` | codec, profile, sample entry tag, pixel format, size, colour range/space/transfer/primaries, frame rate, HDR10 mastering and light-level data, packet count |
| `timestamps` | every packet's display time, in display order; tolerance 1 ms |
| `dv_config` | the Dolby Vision configuration box: profile, level, compatibility id, RPU/EL/BL flags |
| `rpu_present` | RPUs per frame, input against output (an RPU on every frame, or the same gaps in both) |
| `rpu_frames` | each RPU parsed with the `dolby_vision` crate, version 3.3.2 as HISTV pins it: header, mapping (reshaping) data, display management data. Raw bytes are counted, not judged: a re-encode legitimately rewrites them |
| `ext_blocks` | extension blocks per level (L1, L2, L4, L5, L6, ...): frames that differ, lost or gained a block, first frame, largest change per field |
| `packets_md5` | md5 of the video packets (`--mode copy` only) |
| `decode` | full decode of both: frames decoded, error messages |
| `quality` | PSNR and SSIM over a window (`--start`, `--frames`, default frames 0-47) against floors `--min-psnr` 35 dB and `--min-ssim` 0.95 (`--mode reencode`, the default) |

RPUs are paired by display order, not file order. The tool puts an access unit delimiter in front of every packet, counts the RPUs in each, and reports when a packet does not hold exactly one (`rpu_present` says so). Packet n of a stream with B-frames is not frame n.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | every check ran and passed |
| 1 | at least one check is `DIFFERENT` (named in the last line) |
| 2 | nothing Dolby Vision was compared: no RPU check ran |
| 3 | the tool failed: missing binary, unreadable file |

A check that cannot run because the files disagree (different frame counts, a file cut mid-packet) is `DIFFERENT`, never `SKIP`. `--ignore container.FIELD` leaves one container field out of the verdict; `--skip decode,quality` skips checks (a 4K decode takes minutes); `--json FILE` keeps the results.

## Self-test

`run.sh selftest` builds a 2 s synthetic 10-bit clip with B-frames, generates RPUs with `dvc-rpu gen`, joins them with MP4Box, and runs 14 controls. No real video is read. Two undamaged copies must exit 0; each damaged copy must exit 1 and name the check that catches it:

| Damage | Check that must name it |
|---|---|
| RPUs removed (`filter_units=remove_types=62`) | `rpu_present` |
| sample entry changed to `hev1` | `container` |
| one L1 value changed on one frame | `ext_blocks` |
| RPUs attached in decode order, not display order | `ext_blocks` |
| configuration box says 8.4 | `dv_config` |
| timestamps doubled | `timestamps` |
| bytes flipped inside the video data | `packets_md5` |
| last 18 frames cut off | `container` |
| file truncated to 70 percent | `container` |
| re-encode at crf 48 | `quality` |

Also: a re-encode with another GOP whose RPUs were re-attached by display order passes (this is what proves the pairing); no Dolby Vision in either file exits 2; a missing input exits 3. Last run: 14 of 14. Replacing the `rpu_present` comparison with `bad = False` makes the removed-RPU control fail, so the controls can fail.

## Worked example: profile 5, 4K, software libx265

`BL_RPU_dvhe-05_3840x2160@24fps_0_6313.mp4` against HISTV's output (read-only):

```
DIFFERENT container    codec_tag_string dvhe -> dvh1; colour transfer and primaries untagged -> smpte2084 / bt2020
PASS      dv_config    profile=5, level=6, compatibility id 0
PASS      rpu_present  one RPU on each of 6314 frames, both files
DIFFERENT rpu_frames   mapping and display management data differ on 1 of 6314 frames (display 2925); raw bytes identical on 0
DIFFERENT ext_blocks   L1 7 frames, L2 4 frames, L4 104 frames
```

What the numbers mean:

- It is not a pairing artefact. Both files have exactly one RPU per packet; the same frame 2925 comes out of this tool's pairing and of an independent crate-based pairing; on the excerpt below, ffprobe's decoder agrees with the access-unit view on the scene refresh flags of both files.
- HISTV did not rewrite the RPUs. A software libx265 profile 5 job takes the `Native{libx265}` route (`src-tauri/src/encoder.rs:1155-1160`, rule `matches!(profile, 5 | 8 | 10) && caps.dovi_x265`; the arguments are `-dolbyvision 1` plus VBV from `dovi_native_args`, `encoder.rs:1246`, called at `encoder.rs:3121`), so the app passes the RPUs to ffmpeg and touches none. ffmpeg appears to write each RPU afresh: raw bytes are identical on 0 of 6314 frames, and the `dovi_rpu` bitstream filter rewrites 297 of 303 RPUs of the excerpt below.
- The 104 differing frames each carry the RPU of the next display frame: output frame 189 has the L4 block of input frame 190 (anchor_power 182, not 179). At 2925 this includes the next scene's mapping and `scene_refresh_flag`, one frame early. Every one of them is inside a pair that the input stores in reverse order (the later frame decoded first); 104 of the 1273 such pairs with different neighbouring RPUs are affected.
- Reproduced with the pinned ffmpeg n8.1.3 (libavcodec 62.28.103, the encoder tag of the output): the first 12.5 s of the source, `-c:v libx265 -b:v 4000k -maxrate 16000k -bufsize 32000k -dolbyvision 1`, gives the same three frames (189, 237, 285) with the same values. It does not change with `bframes=0`, `frame-threads=1` or a single decoder thread, so it is not x265's own reordering. A parse-and-regenerate round trip of the same RPUs in packet order (`-c copy -bsf:v dovi_rpu=compression=none`) leaves every block identical, so parsing and writing are not at fault. A synthetic profile 8.1 source with B-frames and a different L1 on every frame also survives intact.
- UNVERIFIED: which step hands a frame its neighbour's RPU (the hevc decoder's side data, the frame queue, or the libx265 wrapper); why only some reversed pairs; and whether the one-frame-early mapping at 2925 is visible on a screen. Nothing in this tool or a decoder was changed to find out.

## Files

`run.sh` builds `rpu-dump/` and starts `python3 -m dvcompare`. `dvcompare/media.py` runs ffmpeg and ffprobe, `checks.py` holds the checks, `cli.py` the verdict, `fixtures.py` and `selftest.py` the synthetic clip and the controls. `rpu-dump/` (`dvc-rpu dump | show | gen`) is its own crate, not part of the app's workspace; `dvc-rpu show FILE INDEX` prints one RPU in full, for the first differing frame of a report.

## Known limits

- A plain `ffmpeg -c copy` (ffmpeg 6.1.1 and the pinned n8.1.3 alike, tested without `-strict unofficial`) wrote an MP4 without the Dolby Vision configuration box; `dv_config` reports that as `DIFFERENT`.
- Timestamps are compared as they are: an output that starts later is a difference.
- PSNR and SSIM compare decoded base-layer pictures, not what a Dolby Vision display would show.
- Only the first video stream of each file is compared. Profile 7 dual-layer streams are not covered.
- Linux only; run by hand, no workflow calls it.
