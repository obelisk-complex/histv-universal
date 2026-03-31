# Code Review: HISTV Project

**Reviewed:** 2026-03-31
**Files:** encoder.rs, probe.rs, queue.rs, lib.rs, ffmpeg.rs, events.rs, mkv_tags.rs, cli.rs, cli_main.rs, cli_sink.rs, batch_control.rs, webp_decode.rs, remote.rs, disk_monitor.rs, staging.rs

## Summary

This is a well-structured, high-quality codebase with strong separation of concerns, thoughtful platform abstraction, and good error handling throughout. The architecture (trait-based EventSink/BatchControl, per-file resolution, shared encode loop) is sound. Most findings are efficiency and maintainability improvements rather than correctness issues. The highest-priority items are duplicated code patterns that increase maintenance burden and a few places where cancellation-window race conditions could produce surprising behaviour.

## Findings

### 1. Security

**1.1 Post-command injection surface (encoder.rs:1541-1555)**

The post-command feature passes user-supplied strings directly to `sh -c` (Unix) or `cmd /C` (Windows). This is expected behaviour for a desktop/CLI app where the user controls the input, but worth noting: if HISTV ever gains a mode where job files are received from untrusted sources, this is a direct shell injection vector. No action needed now, but worth a comment.

**1.2 reveal_file / open_file path injection (lib.rs:192-243)**

`reveal_file` passes user-supplied paths to `explorer /select,`, `open -R`, and `xdg-open`. On Windows, `explorer /select,{path}` with a carefully crafted path could in theory invoke unexpected behaviour, though in practice this requires the user to have queued the file themselves. Low risk given the trust model.

No other security issues found.

### 2. Bugs

**2.1 `decide_encode_strategy` copies non-target codecs below threshold (encoder.rs:269-274)**

When `bitrate_mbps <= threshold` and `bitrate_mbps > 0.0` and `is_copyable`, the function returns `Copy` regardless of whether the source codec matches the target codec. This means an MPEG-2 file at 3Mbps with a 5Mbps threshold would be stream-copied rather than transcoded to HEVC, which contradicts the spec comment at line 270-272 ("transcoding to a different codec doesn't make it smaller"). However the inline comment explicitly justifies this ("already small enough"), so this may be an intentional design decision. If it is, the comment should be clearer that cross-codec copy is deliberate for below-threshold files.

**2.2 Race window between cancellation check and process spawn (encoder.rs:1670-1692)**

In `run_ffmpeg_with_progress`, the cancellation check inside the poll loop writes `q` to stdin then waits up to 5 seconds. If `should_cancel_current()` becomes true between the pause-check loop (line 1694-1699) and the next `try_wait` iteration, the cancel is handled correctly. However, if `should_cancel_all()` fires during the brief `sleep(200ms)` at line 1713 *after* the cancellation check at 1679 but before the next loop iteration, there is a 200ms window where the process continues. This is benign (the next iteration catches it) but worth documenting.

**2.3 `peak_additional_bytes_with_delete` uses max-of-one, not cumulative (disk_monitor.rs:98-101)**

The `--delete-source` peak estimate takes the single largest transient file as the peak. This is correct only if files are processed sequentially and the source is deleted before the next file starts. Since the encode loop does delete sources before moving to the next file, this is actually correct - but only because the loop is sequential. If parallel encoding is ever added, this estimate would be wrong. Worth a comment.

**2.4 `CodecFamily::Auto` not handled in `resolve_encoder` (cli_main.rs:852)**

`resolve_encoder` passes `"auto"` as a codec family to the `find()` on `video_encoders`, but no encoder has `codec_family == "auto"`. This falls through to `software_fallback("auto")` which returns `"libx265"` (the `_` arm). This works but is accidental rather than explicit - `Auto` should map to `"hevc"` (or whatever the intended default is) before the lookup.

### 3. Efficiency

**3.1 Duplicated VBR/CQP flag patterns for AV1 encoders (encoder.rs:58-190)**

The `vbr_flags` and `cqp_flags` match arms for AV1 hardware encoders duplicate the exact same flag patterns as their H.264/HEVC counterparts. For example, `av1_amf` produces identical flags to `hevc_amf`/`h264_amf`. These could be collapsed:

```rust
"hevc_amf" | "h264_amf" | "av1_amf" => vec![...],
"hevc_nvenc" | "h264_nvenc" | "av1_nvenc" => vec![...],
```

This would halve the match arm count and eliminate the risk of the AV1 flags drifting from their H.264/HEVC counterparts during future edits.

**3.2 Duplicated MKV tag repair code in three locations**

The MKV lightweight-repair-after-probe pattern appears identically in:
- `lib.rs:296-313` (GUI probe_file command)
- `cli_main.rs:127-145` (CLI batch probe)
- `encoder.rs:848-874` (encode loop pre-decision)

This should be a single function (e.g. `probe_and_repair_tags`) that takes the queue item, file path, and sink, performs the probe, updates the item, and runs the lightweight repair if applicable.

**3.3 Redundant `PathBuf` construction for non-replace output paths (encoder.rs:786-809)**

In the `"beside"` and `"folder"` output path branches, the code creates `out`, converts it to a string, then creates `out2` from that string. This round-trip through `to_string_lossy` is unnecessary and lossy on non-UTF-8 paths. Since both paths are the same in these branches, clone directly:

```rust
let out = out_dir.join(format!("{}.{}", item_base_name, ext));
let out2 = out.clone();
(out, out2)
```

**3.4 `file_progress` in simple (non-TTY) mode prints every 10% boundary on every call (cli_sink.rs:138-141)**

If ffmpeg reports progress at, say, 30.0% on multiple consecutive ticks, `pct % 10 == 0` will print "30%" multiple times. The TTY mode avoids this by using indicatif's position tracking, but simple mode lacks deduplication. Store the last printed percentage and skip if unchanged.

**3.5 `get_system_ram_gb` called once but could be `const`-like (encoder.rs:642)**

`cached_ram_gb` is computed once before the loop, which is good. However, `get_system_ram_gb` on macOS spawns a `sysctl` subprocess every time it's called. The callers outside the loop (`lookahead_for_ram`, `precision_needs_two_pass`) each call it again. Consider caching the result in a `OnceCell` or similar.

**3.6 `BatchSettings` carries five legacy fields (encoder.rs:616-623)**

`video_encoder`, `codec_family`, `audio_encoder`, `audio_cap`, and `output_container` are marked as legacy and not used by the GUI path (which calls `resolve_file_settings` per-file). They add noise to every `BatchSettings` construction site (lib.rs, cli_main.rs). These should be removed when the CLI fully migrates to per-file resolution, which the comments indicate is planned for v2.5.

**3.7 `probe_metadata` and `extract_frames` in webp_decode.rs duplicate RIFF parsing**

Both functions independently parse the RIFF container through VP8X, ANIM, and ANMF chunks. `extract_frames` does everything `probe_metadata` does plus reads frame data. Consider having `extract_frames` optionally return just metadata (or extract a shared parsing core) to avoid the duplicated loop structure.

### 4. Maintainability

**4.1 Inconsistent indentation (tabs vs spaces)**

Several files mix tab and space indentation, most noticeably in `encoder.rs` (e.g. lines 91, 159, 381, 423, 500, 576, 723-724) and `cli.rs` (e.g. lines 223-224, 233-234, 245, 253). This suggests manual edits were applied in an editor with different tab settings. While functional, it makes `git diff` noisy and complicates future patching. A single `rustfmt` pass would normalise everything.

**4.2 Encode loop body is 900+ lines (encoder.rs:629-1561)**

`run_encode_loop` is the longest function in the codebase. The WebP branch, fallback branch, post-encode size check, replace-mode rename, and MKV tag repair are all inlined. Extracting these into named helper functions (e.g. `handle_webp_encode`, `handle_hw_fallback`, `post_encode_size_check`, `handle_replace_mode`) would improve readability and make the main loop's control flow visible at a glance.

**4.3 Magic number: 1.15 threshold hysteresis (encoder.rs:284)**

`bitrate_mbps <= threshold * 1.15` is the "close enough to copy" margin. This 15% tolerance isn't documented, named, or configurable. Define it as a named constant with a comment explaining the rationale.

**4.4 `merge_job_into_args` applies unconditionally (cli_main.rs:867-971)**

The comment at line 873-877 acknowledges that job file values override CLI defaults unconditionally because clap derive doesn't expose "was this flag set?". This means `--job file.json --bitrate 8` silently uses the job file's bitrate if it has one. Consider using clap's `matches.value_source()` (available via `CommandExt::get_matches`) to detect explicit user flags, or document this gotcha prominently in `--help`.

**4.5 `write_log` closure captures `&mut Option<BufWriter>` (encoder.rs:684-689)**

This closure takes `writer` by `&mut` reference, which prevents it from being called while other mutable borrows on the loop's variables are active. This works because every call site is careful, but it means the closure can't be extracted to a free function without also extracting the writer. A minor restructuring (making it a method on a small `LogWriter` wrapper) would be cleaner.

**4.6 `is_animated_webp` check is file-extension-only (encoder.rs:723-724)**

The check `item_video_codec == "webp" && item_full_path.to_lowercase().ends_with(".webp")` is redundant - if the video codec is "webp", the file is necessarily a .webp file (that's how it was probed). The extension check adds nothing. More importantly, a static WebP would also pass this check (static WebPs probe as "webp" via the fallthrough path in probe.rs:96-98). The actual animated-vs-static distinction comes from `crate::webp_decode::probe_webp` returning `Some` vs `None`, which is handled inside `transcode_animated_webp`. This isn't a bug (the WebP pipeline handles statics gracefully) but the variable name `is_animated_webp` is misleading.

### 5. Style

**5.1 `#[allow(dead_code)]` on `is_flatpak` (lib.rs:60)**

This suppress suggests the function might not be called from Rust code (only invoked from the frontend). If so, the annotation is correct, but a brief comment explaining why would help future readers.

**5.2 Inconsistent error message casing**

Some error messages start with uppercase ("Failed to launch ffmpeg"), others with lowercase ("could not determine file duration"). The three-tier error model recommends consistent formatting. Minor, but worth a formatting pass.

**5.3 Empty `eprintln!("")` calls (cli_main.rs:186, 237, 368, 419, 448, 664)**

These could be `eprintln!()` (no argument) which is marginally cleaner. Cosmetic only.

## Recommended actions

1. **Collapse AV1 match arms into their H.264/HEVC counterparts** in `vbr_flags`, `cqp_flags`, and `crf_flags` (encoder.rs). Eliminates ~60 lines of duplication and prevents future drift. Low risk, high value.

2. **Extract the MKV probe-and-repair pattern** into a shared function to eliminate the three-site duplication in lib.rs, cli_main.rs, and encoder.rs. Medium effort, high value for maintainability.

3. **Fix `CodecFamily::Auto` in `resolve_encoder`** (cli_main.rs) to explicitly map to `"hevc"` before the encoder lookup, rather than falling through to the `_` arm of `software_fallback`.

4. **Name the 1.15 threshold hysteresis constant** (encoder.rs:284) and add a brief comment explaining its purpose.

5. **Run `rustfmt`** across the codebase to normalise the mixed tab/space indentation introduced by manual patching.

6. **Extract sub-functions from `run_encode_loop`** for the WebP branch, HW fallback, post-encode size check, and replace-mode handling. This is the highest-effort item but would make the most critical function in the codebase significantly more readable.

7. **Fix non-TTY progress deduplication** in `cli_sink.rs` - track last printed percentage to avoid duplicate "30%" lines.

8. **Cache `get_system_ram_gb`** in a `OnceLock<u64>` so macOS doesn't spawn `sysctl` on every call from outside the encode loop.

9. **Remove or plan removal of legacy `BatchSettings` fields** once the CLI fully uses `resolve_file_settings` per-file. Until then, add a tracking issue reference to the comments.

10. **Consider extracting shared RIFF parsing** in webp_decode.rs so `probe_metadata` and `extract_frames` don't duplicate the container walk.