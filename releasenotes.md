## Honey, I Shrunk The Vids v2.5.3 - Flathub submission release

### Security and compliance
- Licence changed from Unlicense to GPL-3.0-or-later
- FFmpeg GPL licence text installed in Flatpak bundle
- CSP hardened: `unsafe-inline` removed from `script-src` (JS extracted to external file)
- `post_command` blocked from JSON job files (CLI-only, with warning)
- MKV parser bounds caps: `read_string` capped at 1 MB, `read_uint` at 8 bytes
- Windows `open_file` rewritten to avoid `cmd /C start` injection
- Windows `reveal_file` and `open_file` validate path existence and reject UNC paths
- TOCTOU-safe replace mode: backup-rename-delete pattern (local and remote)
- WebKitGTK compatibility: `WEBKIT_DISABLE_DMABUF_RENDERER` and `__NV_DISABLE_EXPLICIT_SYNC` set before GTK init
- `devtools` disabled in release config
- Unused `cdn.jsdelivr.net` removed from CSP

### Performance
- WebP animated decode uses single ffmpeg process (was one per frame)
- Packet scanning uses CSV stream parsing (was multi-hundred-MB JSON DOM)
- NAL reader uses bulk `extend_from_slice` (was byte-by-byte push)
- Hardware encoder detection runs concurrently via `JoinSet` (was sequential)
- Disk space check uses `statvfs` syscall (was `df` subprocess)
- ffmpeg/ffprobe paths use `OnceLock` fast path (was `RwLock` on every call)
- Staging file copy is async (`tokio::fs::copy`, no longer blocks runtime)
- Wave copy-back is async (was blocking `std::fs::copy`)
- `capabilities()` cached in `AtomicBool` (was stat syscall per file)
- `canonicalize_cached` uses syntactic check (was `is_file()` stat before cache)

### Compatibility
- Flatpak runtime upgraded from GNOME 47 to GNOME 50
- HEVC in MP4 now tagged `hvc1` (fixes playback in Safari/iOS/macOS)
- Container-aware audio: Opus/Vorbis in MP4 re-encoded to EAC3 (was silent ffmpeg failure)
- Container-aware subtitles: MP4 uses `mov_text`, bitmap subs silently skipped (was ffmpeg error)
- Container-aware remux fallback: respects MP4 codec constraints
- `CompatCapped` audio uses AAC (was AC3)
- `CompatCapped` handles unknown bitrate (0) by using cap instead of producing `-b:a 0k`
- Non-UTF-8 filenames rejected at queue add time (was silent mangling)
- Windows MAX_PATH: extended-length `\\?\` prefix for paths over 240 chars
- macOS `statvfs` struct layout fixed (was using Linux field sizes)
- macOS FUSE: `macfuse`/`osxfuse` mount types detected, local devices excluded
- `/proc/mounts` octal unescaping handles multi-byte UTF-8 correctly
- `canonicalize` has 5-second timeout on dead network mounts
- Case-insensitive `.mkv` extension check (was missing `.MKV` files)
- `printenv PATH` instead of `echo $PATH` (immune to chatty shell profiles)
- Atomic config write (write to temp, then rename)
- `env::set_var` annotated for Rust 2024 edition migration
- `file_name().unwrap()` replaced with `unwrap_or_default()` in dovi_tools

### Download resilience
- ffmpeg download: multi-mirror fallback chain for all platforms
- GPAC/MP4Box download: dynamic URL discovery via GitHub Releases API
- Archive header validation for dynamically discovered downloads
- All extraction functions use `tempfile::TempDir` (was deterministic names, race condition)
- Invalid macOS x86_64 ffmpeg fallback URL removed (was pointing to ARM64 binary)

### Dead code removal
- Removed 6 unused functions: `describe_decision`, `lookahead_for_ram`, `precision_needs_two_pass`, `format_bytes_signed`, `update_video_stream_tags`, `is_mp4box_available`
- Removed legacy `BatchSettings` comment (fields now actively used)
- Removed dead `if is_mp4 { "eac3" } else { "eac3" }` identical arms
- Removed `applyFlatpakMode()` restrictions (unnecessary with `--filesystem=host`)

### CLI flags wired up
- `--audio` now controls audio strategy (was ignored)
- `--audio-cap` now sets bitrate cap (was hardcoded to 640)
- `--encoder` now forces a specific encoder (was ignored)
- `--codec` now forces codec family for all files (was ignored)
- `--disk-limit` now pauses encoding when disk is full (was ignored)
- Job file fields `disk_resume`, `compat`, `preserve_av1` now load correctly
- `CodecFamily` and `AudioCodec` `FromStr` now accept "auto"

### Tests
- 131 tests (up from 21): `decide_encode_strategy`, `parse_duration_tag`, `parse_numeric`, `build_audio_args_from_probe`, `resolve_file_settings`, `vbr/cqp/crf_flags`, `format_mkv_duration`, `lightweight_repair`, `assemble_ffmpeg_args` (hvc1, faststart, mov_text), MP4 container audio compat, `Canvas::new` overflow, CLI `FromStr` round-trips

### CI/CD rebuilt
- Tests run on every push and PR (`ci.yml`)
- Reusable `build-platform.yml` replaces 4 duplicate patch workflows
- SHA-256 verification on ffmpeg downloads
- Cargo caching via `Swatinem/rust-cache`
- Flatpak SDK cached between builds
- Flathub-grade Flatpak build from source using real manifest with GNOME 50
- `contents: read` permissions on non-release workflows
- hdiutil retry logic for macOS DMG reliability
- Shell injection safety via env vars

### Flatpak / Flathub
- Screenshots added to metainfo (7 images)
- Metainfo licence updated to GPL-3.0-or-later
- Release entry added for v2.5.3
- QuillX AI disclosure badge added to README

### Other
- HTML title updated from v2.4.0 to v2.5.3
- README updated: AC3 references changed to AAC, Flatpak description updated, project structure updated, licence section updated
- Frontend JS extracted from inline `<script>` to `src/js/app.js` (2,664 lines)
- Drag-and-drop and clipboard paste re-enabled in Flatpak (work with `--filesystem=host`)
- `tokio` features trimmed from `full` to specific features
- `dolby_vision`/`hdr10plus` version pins relaxed from `=` to `~`
- Subtitle stream count added to `ProbeResult` (replaces blind 8-stream loop in deep repair)
- Mutex poisoning recovery in CLI sink (`unwrap_or_else(|e| e.into_inner())`)
- WebP `total_duration_ms` uses `saturating_add`
- WebP `padded_size` cast to u64 before addition
- `Ordering::Relaxed` replaced with `Acquire`/`Release` on detection flags
