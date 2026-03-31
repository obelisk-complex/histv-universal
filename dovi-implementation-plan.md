# Dolby Vision and HDR10+ Preservation - Implementation Plan

Reference document for adding automatic Dolby Vision and HDR10+ dynamic metadata preservation to HISTV. Covers detection, tool integration, encoding pipeline changes, UI/CLI adjustments, and testing.

## Design principles

- **Preserve the most we can, degrade gracefully.** One checkbox ("Preserve HDR") controls everything. The app detects the source format, checks what tools are available, and picks the best path automatically.
- **Never surprise the user with silent degradation.** If the best path isn't available, tell them before encoding starts and offer to fix it.
- **No new user-facing complexity.** The user doesn't need to know what DV profile they have or what tools are installed.
- **Don't change what doesn't need changing.** The output container defaults to "Auto" (matching the source format where possible). DV files override to MP4 when required, but everything else keeps its original container.

## Encoding cascade

When HDR is ticked (preserve), the encoder picks the highest tier it can for each file:

| Tier | Source | Tools needed | Result |
|------|--------|--------------|--------|
| 1 | Dolby Vision (any profile) | `dolby_vision` crate + MP4Box | Full DV preservation (extract RPU, encode, inject, package with DV flags). Output forced to MP4. |
| 2 | HDR10+ | `hdr10plus_tool` crate | Full dynamic metadata preservation (extract metadata, encode, inject back). |
| 3 | DV Profile 8/7 (with HDR10 fallback) | None | DV layer stripped, HDR10 base layer preserved. Current behaviour - PQ transfer and BT.2020 primaries survive a normal encode. |
| 4 | DV Profile 5 (no HDR10 fallback) | None | Encoded as HDR10 without mastering metadata. Watchable but display-dependent. Pre-flight warning offered. |
| 5 | HDR10 / HLG | None | Preserved as-is. Current behaviour. |

When HDR is unticked, everything tonemaps to SDR via Hable regardless of source type. No change from current behaviour.

## Tool dependencies

### Rust crates (compiled in)

- **`dolby_vision`** - quietvoid's library for RPU extraction, injection, and profile conversion. Equality-pinned in Cargo.toml (`dolby_vision = "=x.y.z"`). Bumped manually after testing.
- **`hdr10plus_tool`** - quietvoid's library for HDR10+ metadata extraction and injection. Same pinning strategy.

These are compiled into the HISTV binary. No external files to ship or discover per platform. The trade-off vs forking: upstream tracks DV/HDR10+ spec changes; pinning gives stability without maintenance burden. Fork only if upstream is abandoned.

### External binary

- **MP4Box** (GPAC project) - required for writing DV-flagged MP4 containers. No Rust crate equivalent exists.
  - Discovered next to the HISTV binary (same location as ffmpeg).
  - Bundled in `-full` builds and flatpak.
  - Auto-download offered if absent and needed (same UX pattern as ffmpeg downloader).

## Implementation batches

### Batch A: Probe enhancement

**Files:** `probe.rs`, `queue.rs`

Extend the existing single-pass ffprobe JSON to extract DV/HDR10+ side data.

1. **Extract DOVI configuration record from ffprobe side data.** The existing `show_streams` call already returns side data when present. Look for `side_data_list` entries where `side_data_type == "DOVI configuration record"`. Extract `dv_profile`, `dv_bl_signal_compatibility_id`, and `dv_level`.

2. **Detect HDR10+ via side data.** Look for `side_data_type == "HDR Dynamic Metadata SMPTE2094-40 (HDR10+)"` or equivalent. A boolean `has_hdr10plus` is sufficient - we don't need the metadata content at probe time.

3. **Extend `ProbeResult`.** Add:
   ```rust
   pub dovi_profile: Option<u8>,
   pub dovi_bl_compat_id: Option<u8>,
   pub has_hdr10plus: bool,
   ```

4. **Extend `QueueItem`.** Mirror the new `ProbeResult` fields so the encoding engine and pre-flight checks can read them without re-probing.

5. **Verify with test files.** Grab DV Profile 5, 7, and 8 samples (see Testing section). Confirm ffprobe returns the expected side data and the probe populates the new fields correctly.

**Acceptance criteria:** `probe_file` correctly identifies DV profile/compat and HDR10+ presence for all test files. Non-HDR and standard HDR10/HLG files return `None`/`false` for the new fields with no regression.

### Batch B: Tool discovery and download infrastructure

**Files:** new `dovi_tools.rs`, `ffmpeg.rs` (reference pattern)

Build the discovery and download system for MP4Box, mirroring the existing ffmpeg infrastructure.

1. **Create `dovi_tools.rs` module.** Follows the same pattern as `ffmpeg.rs`:
   - `init()` function called at startup to resolve MP4Box binary path.
   - Resolution order: next to HISTV binary, app-data bin dir, well-known dirs, PATH.
   - `is_available()` async function that runs `MP4Box -version`.
   - `mp4box_command()` that returns a configured `Command` (with `hide_window` on Windows).

2. **Crate availability check.** Since `dolby_vision` and `hdr10plus_tool` are compiled in, they're always available. Add a simple function that reports which capabilities are present:
   ```rust
   pub struct DoviCapabilities {
       pub can_process_dovi: bool,     // dolby_vision crate - always true
       pub can_process_hdr10plus: bool, // hdr10plus_tool crate - always true
       pub can_package_dovi_mp4: bool,  // MP4Box binary found
   }
   ```
   Tier 1 (full DV preservation) requires `can_process_dovi && can_package_dovi_mp4`. Tier 2 (HDR10+) requires only `can_process_hdr10plus` (no MP4Box needed - metadata is injected into the HEVC bitstream, container is unchanged).

3. **Download function.** `download_mp4box()` following the ffmpeg downloader pattern:
   - Platform-specific download URLs for GPAC static builds.
   - Stream download with progress reporting via `EventSink`.
   - Extract binary to target directory, set executable permissions on Unix.
   - `reinit()` after download to update cached path.

4. **Wire into GUI init.** Call `dovi_tools::init()` alongside `ffmpeg::init()` at app startup. Log resolved paths.

5. **Wire into CLI init.** Same discovery, no download capability (CLI users install tools themselves, consistent with the existing CLI ffmpeg policy).

**Acceptance criteria:** MP4Box is discovered if present, reported as missing if not. Download function works on all three platforms. `DoviCapabilities` accurately reflects what's available.

### Batch C: Pre-flight check and GUI modal

**Files:** new GUI modal in `index.html`/`app.css`, Tauri command in `lib.rs`, encoder pre-flight in `encoder.rs`

The pre-flight check runs when the user clicks Start, before any encoding begins.

1. **Pre-flight scan function.** Walk the pending queue items. For each file, determine the best available tier from the cascade. Collect files that won't get their best-possible encode into a report:
   ```rust
   pub struct PreflightWarning {
       pub file_name: String,
       pub source_type: String,        // "Dolby Vision Profile 8", "HDR10+", etc.
       pub best_possible: String,      // "Full DV preservation"
       pub actual_outcome: String,     // "HDR10 fallback (DV tools not found)"
       pub missing_tool: Option<String>, // "MP4Box"
   }
   ```

2. **GUI modal.** Triggered by the pre-flight scan if warnings exist. Layout:
   - Header: "Some files need additional tools for best results"
   - Table listing affected files, what they'll lose, and why.
   - Three buttons: **Download tools** / **Encode anyway** / **Cancel**
   - If DV Profile 5 files are present without tools: additional row explaining the SDR conversion option, with a per-file checkbox to convert those specific files to SDR.
   - Download button grabs all missing tools in one pass (single progress bar), then auto-starts encoding on completion.

3. **Tauri command.** `preflight_check` command that runs the scan and returns the warnings as JSON. Frontend calls this on Start click, shows modal if non-empty, proceeds directly if empty.

4. **CLI behaviour.** No modal. Log each warning at the start of the dry-run table. Proceed with best available path. `--no-hdr` forces SDR for everything (existing flag, no change).

**Acceptance criteria:** Modal appears only when needed. Download + auto-start works. Empty queue or queue with no affected files skips the modal entirely. CLI logs warnings without prompting.

### Batch D: DV RPU extract/inject pipeline

**Files:** new `dovi_pipeline.rs`, `encoder.rs` (integration)

The core DV processing pipeline using the `dolby_vision` crate.

1. **RPU extraction.** Given a source file path, demux the raw HEVC bitstream (via ffmpeg `-c:v copy -bsf:v hevc_mp4toannexb`), then use the `dolby_vision` crate to parse NAL units and extract the RPU data. Store RPU data in memory (no intermediate file).

2. **RPU injection.** After encoding, demux the encoded HEVC bitstream, inject the RPU data back into the NAL units using the `dolby_vision` crate. Write the result to a temp file.

3. **Profile conversion.** For Profile 7 without HDR10 fallback (`dovi_bl_compat_id` indicates no fallback), convert to Profile 8.1 during injection (the crate supports this via mode 2 conversion).

4. **MP4Box packaging.** Take the injected HEVC bitstream + original audio/subtitle streams and package into an MP4 with DV container flags: `MP4Box -add video.hevc:dvp=8.1:dv-cm=hdr10 -add audio.aac output.mp4` (simplified; actual command varies by profile).

5. **Integration with encoder loop.** In `encoder.rs`, after the existing per-file encoding decision:
   - If file is DV and tools available: run pre-encode RPU extraction, normal encode, post-encode RPU injection + MP4Box packaging.
   - The existing ffmpeg encode step is unchanged.
   - The intermediate files (raw HEVC bitstreams) are temp files, cleaned up after packaging.
   - Output container is forced to MP4 for DV files regardless of user selection. Log: "Dolby Vision requires MP4 container - output set to MP4 for this file."

6. **Error handling.** If any DV pipeline step fails, fall back to the next tier (HDR10 base layer) and log the failure. Never fail the entire file because of a DV processing error.

**Acceptance criteria:** Round-trip RPU extract/inject produces valid DV files (verified with `dovi_tool verify` or mediainfo). Profile 7 to 8.1 conversion works. MP4Box packaging produces files that play correctly on DV-capable displays. Fallback on error works cleanly.

### Batch E: HDR10+ metadata preserve pipeline

**Files:** extend `dovi_pipeline.rs` or new `hdr10plus_pipeline.rs`, `encoder.rs` (integration)

1. **Metadata extraction.** Before encoding, use the `hdr10plus_tool` crate to extract dynamic metadata from the source HEVC bitstream. Store in memory as the crate's native structure.

2. **Metadata injection.** After encoding, inject the metadata back into the encoded HEVC bitstream's SEI NAL units.

3. **Integration.** Same pattern as DV: pre-encode extract, normal ffmpeg encode, post-encode inject. No MP4Box step needed - HDR10+ metadata lives in the HEVC bitstream, not the container.

4. **No container constraint.** HDR10+ works in both MKV and MP4. User's container selection is respected.

5. **Error handling.** Same principle: if injection fails, fall back to HDR10 (static metadata only) and log.

**Acceptance criteria:** HDR10+ metadata survives a re-encode. `hdr10plus_tool verify` confirms valid metadata in the output. Fallback on error works.

### Batch F: GUI surface changes

**Files:** `index.html`, `app.css`, `config.rs`

Minimal UI changes - the cascade is automatic, so the GUI mostly just needs to show what's happening.

1. **Queue table.** Add a column or badge showing the detected HDR type: "DV8", "DV5", "DV7", "HDR10+", "HDR10", "HLG", "SDR". Populated from the new probe fields.

2. **HDR checkbox tooltip.** Update to mention DV/HDR10+ preservation: "When ticked, HDR files stay HDR. Dolby Vision and HDR10+ dynamic metadata are preserved when tools are available. When unticked, all HDR content is converted to SDR with industry-standard tonemapping."

3. **Log messages.** The encoding engine already logs via `sink.log()`. Add clear per-file messages:
   - "Dolby Vision Profile 8 detected - full preservation via RPU extract/inject"
   - "HDR10+ detected - preserving dynamic metadata"
   - "Dolby Vision Profile 8 detected - DV tools not found, falling back to HDR10"
   - "Dolby Vision Profile 5 detected - encoding as HDR10 (no mastering metadata)"
   - "Dolby Vision requires MP4 container - output set to MP4 for this file"

4. **Auto container option.** Add "Auto" as a third option in the `sel-container` dropdown (value `"auto"`), and make it the new default. Tooltip: "Keeps the original container format where possible. Files in formats that don't support modern codecs (AVI, WMV, etc.) default to MKV. Dolby Vision files always output as MP4 regardless of this setting."

5. **Container display in queue.** The target bitrate column and any other display that references the output container must resolve "auto" per-file against the source path rather than reading a single global string. Extract a shared `resolve_container(source_path, setting, is_dovi_tier1)` helper that both the display logic and the encoder can call.

6. **Config round-trip.** Ensure `"auto"` saves to and loads from `config.json` correctly. Existing configs without this value continue to work (MKV/MP4 selections are unchanged).

**Acceptance criteria:** Queue shows correct HDR type badges. Tooltip is accurate. Log messages match the actual encoding path. Auto container resolves correctly per-file in the queue display. Config saves and loads "auto" without breaking existing configs.

### Batch G: CLI surface changes

**Files:** `cli.rs`, `cli_main.rs`

1. **Dry-run table.** Add HDR type to the per-file display (alongside the existing resolution/codec/action columns). Show "DV8", "HDR10+", etc.

2. **Auto container default.** Change `--container` default from `mkv` to `auto`. Accept `auto`, `mkv`, and `mp4` as valid values. The dry-run table should show the resolved container per-file (not just "auto") so the user sees exactly what will happen.

3. **Warnings.** Before encoding starts (after the dry-run table), log warnings for files that won't get best-possible treatment:
   ```
   WARNING: 2 Dolby Vision files will fall back to HDR10 (MP4Box not found)
   WARNING: 1 DV Profile 5 file will be encoded as HDR10 without mastering metadata
   ```

4. **Job file.** Add `"auto"` as a valid value for the `container` field. No other new fields needed. The existing `hdr` boolean controls everything, matching the GUI's single-checkbox design.

**Acceptance criteria:** Dry-run output shows HDR types and resolved container per-file. Auto is the default. Warnings appear when tools are missing. Job files accept "auto".

### Batch H: Build and distribution

**Files:** `Cargo.toml`, workflow YAML files, flatpak config

1. **Cargo.toml.** Add `dolby_vision` and `hdr10plus_tool` as dependencies with equality pins. Gate behind a feature flag (e.g., `dovi`) that's enabled by default for both GUI and CLI builds. This allows building without DV support if needed (e.g., if the crates cause issues on an exotic platform).

2. **`-full` builds.** Add MP4Box to the ffmpeg download step in the CI workflows. Package alongside ffmpeg in the zip/dmg/AppImage.

3. **Flatpak.** Include MP4Box in the flatpak build alongside ffmpeg. Update the metainfo XML to mention DV/HDR10+ support.

4. **Compile time check.** The `dolby_vision` and `hdr10plus_tool` crates are pure Rust with no C dependencies, so they should compile cleanly on all targets. Verify in CI.

**Acceptance criteria:** All six build targets compile and pass. `-full` builds include MP4Box. Flatpak includes MP4Box. Feature flag allows building without DV support.

### Batch I: Output container handling

**Files:** `encoder.rs`, new shared helper (or inline in encoder)

Container resolution is now per-file. Three layers of logic applied in order:

1. **Auto resolution.** When the container setting is `"auto"`, derive from the source file's extension:
   - `.mkv` → MKV
   - `.mp4`, `.m4v` → MP4
   - Everything else (AVI, TS, WMV, MOV, WebM, FLV, VOB, etc.) → MKV (can hold anything)

   When the container setting is `"mkv"` or `"mp4"`, use that directly (existing behaviour).

2. **DV override.** When a file enters the DV pipeline (Tier 1), override the resolved container to MP4 regardless of auto or explicit selection. Log it: "Dolby Vision requires MP4 container - output set to MP4 for this file."

3. **Replace mode.** If the source is an MKV and the output is forced to MP4 (by DV override), the replacement will change the extension. Log it: "Source was MKV, output is MP4 (Dolby Vision requires MP4 container)."

4. **Subtitle handling.** MP4 has limited subtitle support compared to MKV (no PGS/VobSub). If the resolved container is MP4 (whether by auto, explicit selection, or DV override) and the source has bitmap subtitles, log a warning: "Bitmap subtitles (PGS/VobSub) cannot be carried in MP4 - these tracks will be dropped." This is a known limitation, not a bug.

5. **Shared resolution function.** Extract `resolve_container(source_path: &str, setting: &str, is_dovi_tier1: bool) -> &str` as a shared helper. Called by:
   - The encoder loop (to determine the actual output path)
   - The GUI queue display (to show the correct target container per-file)
   - The CLI dry-run table (to show the resolved container per-file)

   This function must be deterministic and cheap - no I/O, just string matching on the source extension and the setting value.

**Acceptance criteria:** Auto resolves correctly for all supported input extensions. DV files always output as MP4. Non-DV files respect auto/explicit selection. Replace mode handles extension changes. Subtitle warnings appear when relevant. The shared resolution function is used consistently across GUI display, CLI dry-run, and the encoder.

## Testing

### Test file matrix

| Profile | Source | Where to get it |
|---------|--------|-----------------|
| DV Profile 5 | Single-layer, no HDR10 fallback | Kodi wiki samples, Dolby Developer test kit |
| DV Profile 7 | Dual-layer with HDR10 fallback | Kodi wiki FEL test samples |
| DV Profile 8 | Single-layer with HDR10 fallback | 4kmedia.org LG demos, Demolandia samples |
| HDR10+ | Samsung-originated content | Kodi wiki samples |
| HDR10 | Standard PQ transfer | Existing test files |
| HLG | BBC-style broadcast HDR | Kodi wiki samples |
| SDR | Control group | Existing test files |

### Test scenarios per file

1. **Detection.** Probe correctly identifies format, profile, and compat ID.
2. **Tier 1 (tools present).** Full DV/HDR10+ preservation. Verify output with mediainfo and `dovi_tool verify`.
3. **Tier 3/4 (tools absent).** Remove MP4Box, re-encode. Verify graceful fallback to HDR10.
4. **SDR tonemap.** Untick HDR, encode. Verify Hable tonemap produces correct SDR output.
5. **Pre-flight modal.** Queue mixed content with tools absent. Verify modal lists correct files and actions.
6. **Download flow.** Start with no MP4Box. Trigger download from modal. Verify encoding starts automatically on completion.
7. **Error recovery.** Corrupt an intermediate file mid-pipeline. Verify fallback to next tier, not a hard failure.
8. **Auto container - MKV source.** Set container to Auto, encode an MKV. Output should be MKV.
9. **Auto container - MP4 source.** Set container to Auto, encode an MP4. Output should be MP4.
10. **Auto container - other source.** Set container to Auto, encode an AVI/TS/WMV. Output should be MKV.
11. **Auto container - DV override.** Set container to Auto, encode a DV MKV with tools present. Output should be MP4 (DV override wins). Log message confirms the override.
12. **Auto container - queue display.** Add mixed MKV/MP4/AVI files with Auto selected. Queue display and CLI dry-run show the correct resolved container per-file, not "auto".

### Verification tools

- **mediainfo** - confirm DV profile, HDR10+ presence, colour metadata in output files.
- **`dovi_tool verify`** - validate RPU data integrity after round-trip.
- **`hdr10plus_tool verify`** - validate HDR10+ metadata integrity.
- **ffprobe** - confirm side data in output matches expectations.

## Risk assessment

| Risk | Impact | Mitigation |
|------|--------|------------|
| `dolby_vision` crate API changes | Build breaks on version bump | Equality pin; only bump after testing |
| `dolby_vision` crate abandoned | No upstream spec tracking | Fork from pinned version at that point |
| MP4Box download URLs change | Auto-download breaks | Version-pin URLs; monitor GPAC releases |
| RPU extraction fails on unusual DV content | File fails to encode | Graceful fallback to HDR10; never hard-fail |
| Compile time increase from new crates | Slower CI | Feature-flag allows excluding if needed |
| MP4 container forced for DV drops bitmap subs | User loses subtitle tracks | Log warning; document in README |

## Version and release

This is a major feature addition. Target version **v2.4.0**. Ship as a standard release (not prerelease) once the test matrix passes. Update README, patchnotes, and CLI-README to document the new capability.

Batches A and B are independent and can be developed in parallel. C depends on A and B. D and E depend on A. F and G depend on D, E, and I (for the shared container resolution function). H can start alongside D/E. I depends on D but the auto-resolution logic (items 1 and 5) has no DV dependency and can be built early - only the DV override (item 2) needs D.