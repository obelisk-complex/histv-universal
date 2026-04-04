# Honey, I Shrunk The Vids

[![Quillx](https://raw.githubusercontent.com/qainsights/quillx/main/badges/quillx-4.svg)](https://github.com/qainsights/quillx) Architecture, integration decisions, and review are mine. Claude generated the implementation under close direction. Output reviewed line by line (but that doesn't mean I caught everything).

A cross-platform batch video encoder that shrinks your video files to a target quality level. No need to babysit the queue or tweak settings per file - just pick a target bitrate, add your files, and hit start.

Available as a desktop app and a headless CLI for servers (see [CLI-README.md](CLI-README.md)) The CLI can be used with Sonarr/Radarr via Custom Script; please see the [Sonarr-Radarr Guide](Sonarr-Radarr-Integration.md) for details. Both GUI and CLI use the same encoding engine for compatibility and maintainability. Portable binaries are on the GitHub [Releases page](https://github.com/obelisk-complex/histv-universal/releases).

---

### Screenshots
<details>

![](/screenshots/gui-1.png)

![](/screenshots/gui-2.png)

![](/screenshots/gui-3.png)

![](/screenshots/gui-4.png)

![](/screenshots/gui-5.png)

![](/screenshots/gui-6.png)

![](/screenshots/cli-1.png)

![](/screenshots/cli-2.png)

![](/screenshots/cli-3.png)

</details>

## Features

<details>
<summary><strong>Full feature list</strong></summary>

- **Queue management** - drag-and-drop, clipboard paste (Ctrl+V), file/folder picker. Click, Shift+click, Ctrl+click, Ctrl+A, Delete. Right-click context menu (also Shift+F10 for keyboard users). Drag to reorder.
- **GIF/APNG/WebP support** - animated GIFs, APNGs, and WebPs are converted to proper video files automatically.
- **Per-file codec resolution** - the app decides the codec, container, and audio handling for each file individually based on its source properties. H.264 stays H.264, HEVC stays HEVC, and everything else converts to HEVC. No manual codec selection needed.
- **AV1 support** - first-class AV1 encoding via libsvtav1 and hardware AV1 encoders (AMF, NVENC, QSV, VideoToolbox, VAAPI). Enable Preserve AV1 to keep AV1 sources as AV1 instead of converting to HEVC.
- **Compatibility Mode** - one checkbox to force H.264/MP4/AAC output for maximum device compatibility.
- **Smart decisions** - the app figures out what to do with each file before you start. Files that are already small enough get copied untouched. Files that are too big get shrunk. The queue shows the plan in real time as you change settings.
- **GPU encoding** - tests your GPU at startup and picks the fastest working encoder for each codec family (HEVC, H.264, AV1). Falls back to software automatically if the GPU encoder fails mid-file.
- **QP / CRF quality modes** - two ways to control quality when files are below your target bitrate. QP gives predictable sizes; CRF gives better-looking results with less predictable sizes.
- **Precision mode** - one checkbox for the best possible quality. Uses software encoding with smart analysis that adapts to your system's RAM. Tests a few short clips first to make sure it won't make the file bigger. Slower, but produces the smallest file that still looks great.
- **VBR peak ceiling** - controls how much the bitrate can spike on action-heavy scenes (1.5x to 3x). Higher values look better on complex content.
- **Audio handling** - each audio track is handled separately. Tracks already below 640kbps are copied as-is. Tracks above the cap are re-encoded to the same codec at the cap. Codecs with no ffmpeg encoder (DTS, TrueHD) fall back to EAC3. Compatibility Mode forces AAC.
- **HDR support** - HDR videos are detected automatically and encoded in 10-bit. Dolby Vision and HDR10+ dynamic metadata are preserved automatically when tools are available (the app downloads MP4Box if needed for DV packaging). When preservation isn't possible, the app falls back gracefully to HDR10 and tells you why. Untick the HDR checkbox to convert HDR to SDR with industry-standard tonemapping so colours look right on a normal screen.
- **Pre-flight checks** - before encoding starts, the app scans your queue and warns you if any files won't get their best-possible encode (e.g. DV files without MP4Box). Offers to download missing tools, encode anyway, or cancel.
- **Subtitles** - all subtitle tracks are kept.
- **Output options** - save to a folder, next to the source file, or replace the source. Container is preserved per-file (MKV stays MKV, MP4 stays MP4, MOV stays MOV; other formats default to MKV).
- **Queue columns** - source file size, estimated compressed size, resolution, HDR type badge (DV8, HDR10+, HDR10, HLG, SDR), source and target bitrates, and resizable column headers.
- **Size safety net** - if the encoded file ends up meaningfully bigger than the original (accounting for container overhead based on frame rate and audio streams), the app throws it away and copies the original into the new container instead. Files already below the target threshold are copied without re-encoding.
- **MKV tag repair** - detects and corrects stale stream statistics tags left behind by ffmpeg and third-party muxing tools, so bitrate decisions use the real numbers. Runs automatically at import and after each encode, with optional manual deep repair for severely corrupted metadata.
- **Performance controls** - limit CPU threads and/or run encoding at low priority so your PC stays usable.
- **ETA** - shows estimated time remaining in the progress bar and the window title, so you can see it from the taskbar.
- **Batch controls** - start, pause, cancel current file, cancel everything. Progress bars per file and overall. Files added mid-batch are picked up automatically.
- **Dry run** - shows what the app would do without actually encoding anything.
- **Network drive support** - detects files on network shares and stages them locally in waves before encoding, so slow networks don't bottleneck the encoder. Wave planning groups files to fit available staging space, stages an entire wave at once, encodes, cleans up, and repeats.
- **Disk space monitoring** - pauses encoding if your drive gets too full, resumes when space frees up.
- **Post-batch actions** - shut down, sleep, log out, or run a custom command when the batch finishes.
- **Log console** - colour-coded log with filters, optional file export.
- **Notifications** - system notification when the batch finishes.
- **Auto-clear** - optionally clears finished items from the queue.
- **Flatpak support** - available as a Flatpak bundle for Linux with host filesystem access so ffmpeg subprocesses can reach your files directly.
- **Themes** - six built-in themes (dark, light, and four community-inspired palettes). Create your own with just 6 colour values - the app derives everything else automatically. See [THEMES.md](THEMES.md).
- **ffmpeg downloader** - offers to download ffmpeg for you if it's not installed.
- **Remembers settings** - everything is saved to config.json in the same folder as the .exe, and restored on next launch.
- **CLI version** - same engine as a command-line tool for servers and scripts. Shows a persistent queue table during encoding matching the GUI's columns. See [CLI-README.md](CLI-README.md).

</details>

## Download

Grab the latest build from the GitHub **[Releases page](https://github.com/obelisk-complex/histv-universal/releases)**.

Each platform has a standard build (you provide ffmpeg) and a **-full** build (ffmpeg included). The full builds are larger but work out of the box with no extra setup.

**Standard builds** - ffmpeg not included:

| Platform | GUI | CLI |
|----------|-----|-----|
| **Windows** | `histv-windows.exe` | `histv-cli-windows.exe` |
| **Linux** | `.AppImage` | `histv-cli-linux` |
| **macOS (Apple Silicon)** | `.dmg` (arm64) | `histv-cli-macos-arm64` |
| **macOS (Intel)** | `.dmg` (x64) | `histv-cli-macos-x64` |

**Full builds** - ffmpeg included, no extra dependencies:

| Platform | GUI | CLI |
|----------|-----|-----|
| **Windows** | `histv-windows-full.zip` | `histv-cli-windows-full.zip` |
| **Linux** | `histv-linux-full.AppImage` | `histv-cli-linux-full.tar.gz` |
| **macOS (Apple Silicon)** | `histv-macos-arm64-full.dmg` | `histv-cli-macos-arm64-full.tar.gz` |
| **macOS (Intel)** | `histv-macos-x64-full.dmg` | `histv-cli-macos-x64-full.tar.gz` |

**Flatpak** - sandboxed Linux build with ffmpeg included:

| Platform | GUI |
|----------|-----|
| **Linux** | `histv-linux.flatpak` |

The Flatpak bundles ffmpeg and has host filesystem access for encoding. The CLI is not included in the Flatpak.

All binaries are portable - no installation needed. On Linux, mark the AppImage executable (`chmod +x`). On macOS, open the DMG and drag to Applications. On Windows, extract the zip to a folder.

If you use a standard build, ffmpeg is required. The GUI offers to download it on first launch. The CLI expects it on your PATH (`apt install ffmpeg`, `brew install ffmpeg`, `choco install ffmpeg`, etc.).

| OS | Architecture | GPU encoders |
|----|-------------|-------------|
| Windows | x86_64 | AMF, NVENC, QSV |
| Linux | x86_64 | VAAPI, NVENC, QSV |
| macOS | x86_64 / ARM64 | VideoToolbox |

Hardware encoder detection covers HEVC, H.264, and AV1 codec families on all platforms. Each encoder is verified with a test encode at startup.

<details>
<summary><strong>Building from source</strong></summary>

Requires Rust (stable) and the Tauri v2 CLI. See the [Tauri prerequisites guide](https://v2.tauri.app/start/prerequisites/) for platform-specific dependencies.

```bash
# GUI
cargo install tauri-cli --version "^2"
cargo tauri build

# CLI (no GUI/Tauri/WebKit dependencies)
cargo build --manifest-path src-tauri/Cargo.toml --release --bin histv-cli --no-default-features --features cli
```

</details>

## How it works

You set a target bitrate and the app decides what to do with each file:

| Situation | What happens |
|-----------|-------------|
| Already small enough, same codec family | **Copied as-is** - no re-encoding |
| Already small enough, different codec | **Re-encoded for quality** using QP or CRF into the target codec |
| Too big | **Shrunk** to hit the target bitrate using VBR encoding |
| Zero bitrate / unreadable header | **Re-encoded for quality** using QP or CRF |
| GIF, APNG, or animated WebP | **Always re-encoded** into a proper video |

The target codec is determined per-file: H.264 sources stay H.264, HEVC sources stay HEVC, and everything else (MPEG-2, VP9, etc.) converts to HEVC. With Preserve AV1 enabled, AV1 sources stay AV1. With Compatibility Mode, everything becomes H.264/MP4.

Files within 15% above the threshold that are already in the target codec are also copied rather than re-encoded, to avoid wasting time on marginal gains.

If an encode makes the file meaningfully bigger than it was (accounting for container overhead), the app throws away the encode and copies the original into the new container instead. Small size increases from container format differences are tolerated.

### Dolby Vision and HDR10+

When "Preserve HDR" is ticked, the app automatically detects Dolby Vision and HDR10+ content and preserves the dynamic metadata through re-encoding. No extra settings needed — the app picks the best path for each file:

| Source | Tools needed | Result |
|--------|-------------|--------|
| Dolby Vision (any profile) | MP4Box | Full DV preservation. Output forced to MP4. |
| HDR10+ | None | Full dynamic metadata preservation. |
| DV without tools | None | Falls back to HDR10 base layer. |
| HDR10 / HLG | None | Preserved as-is (existing behaviour). |

All bitstream processing uses streaming I/O so memory usage stays bounded regardless of file size. If a DV/HDR10+ pipeline step fails, the app falls back to the next best option and logs why.

### Precision mode

One checkbox for the best quality the app can produce. It picks the smartest encoding strategy based on how much RAM your system has:

| Your RAM | What it does |
|----------|-------------|
| 16GB or more | Looks 250 frames ahead to plan bitrate (single pass) |
| 8-16GB | Looks 120 frames ahead to plan bitrate (single pass) |
| Under 8GB | Scans the whole file first, then encodes (two passes) |

Before starting the full encode, it tests a few short clips to make sure the result won't end up bigger than the original. If it would, the app switches to a safer mode automatically.

### Who HISTV is for

People with a pile of video files at different sizes and formats who want to shrink the big ones without fiddling with every file individually. Set the target, add the files, walk away.

### How HISTV compares to other tools
<details>

**vs. HandBrake** - HandBrake is built for tweaking every file individually. HISTV is built for doing a whole folder at once without thinking about it. It figures out per-file whether to shrink, copy, or re-encode. If you want fine control over every setting on every file, HandBrake is the better tool.

**vs. Tdarr** - Tdarr runs as a server with plugins, Docker, and a web dashboard. HISTV is an app you open, use, and close.

**vs. writing your own ffmpeg script** - HISTV handles the parts that are annoying to script: figuring out what each file needs, picking the right encoder, falling back when hardware fails, prompting for overwrites, pausing/resuming, and cleaning up after itself.

</details>

<details>
<summary><strong>Project structure</strong></summary>

```
├── src/                        # Frontend (HTML + CSS + JS)
│   ├── index.html
│   ├── js/app.js
│   └── css/app.css
├── src-tauri/
│   ├── Cargo.toml
│   ├── tauri.conf.json
│   ├── build.rs
│   └── src/
│       ├── main.rs             # GUI entry point
│       ├── cli_main.rs         # CLI entry point
│       ├── lib.rs              # Tauri commands, app setup, shared state
│       ├── encoder.rs          # Encoder detection, flag mapping, batch loop
│       ├── ffmpeg.rs           # Binary resolution, download, spawning
│       ├── probe.rs            # Media probing via ffprobe (DV/HDR10+ detection)
│       ├── queue.rs            # Queue data structures, file collection
│       ├── dovi_pipeline.rs    # Dolby Vision RPU extract/inject pipeline
│       ├── dovi_tools.rs       # MP4Box discovery, download, capabilities
│       ├── hdr10plus_pipeline.rs  # HDR10+ metadata extract/inject pipeline
│       ├── hevc_utils.rs       # Streaming HEVC NAL reader/writer
│       ├── mkv_tags.rs         # MKV stream statistics tag repair (EBML)
│       ├── webp_decode.rs      # Animated WebP RIFF parser + decode pipeline
│       ├── events.rs           # EventSink + BatchControl traits
│       ├── config.rs           # GUI persistent settings
│       ├── themes.rs           # Theme loading + built-in themes
│       ├── tauri_sink.rs       # GUI EventSink (Tauri emit)
│       ├── tauri_batch_control.rs  # GUI BatchControl (Mutex + emit)
│       ├── cli.rs              # CLI arg parsing (clap) + job files
│       ├── cli_sink.rs         # CLI EventSink (stderr + indicatif)
│       ├── batch_control.rs    # CLI BatchControl (atomics + TTY prompts)
│       ├── remote.rs           # Network mount detection + caching
│       ├── staging.rs          # Wave-based local staging for remote files
│       └── disk_monitor.rs     # Disk-space estimation + runtime monitoring
├── .github/workflows/          # CI: per-platform builds
├── THEMES.md                   # Theme creation guide
├── CLI-README.md               # CLI documentation
├── Sonarr-Radarr-Integration.md
└── README.md
```

</details>

### Licence

[GPL-3.0-or-later](LICENSE)

### Third-party software

The **-full** builds bundle [FFmpeg](https://ffmpeg.org) and [MP4Box](https://gpac.io) (GPAC). FFmpeg is licensed under the [GNU General Public Licence v2+](https://www.gnu.org/licenses/old-licenses/gpl-2.0.html). The bundled FFmpeg binaries are unmodified static GPL builds from [BtbN/FFmpeg-Builds](https://github.com/BtbN/FFmpeg-Builds) (Windows/Linux) and [evermeet.cx](https://evermeet.cx/ffmpeg/) (macOS). MP4Box is licensed under the [GNU Lesser General Public Licence v2.1+](https://www.gnu.org/licenses/old-licenses/lgpl-2.1.html). The corresponding source code is available from those repositories. We will provide the source on request for three years from the date of each release - file an issue on this repo.

Dolby Vision processing uses the [dolby_vision](https://crates.io/crates/dolby_vision) crate by quietvoid (MIT). HDR10+ processing uses the [hdr10plus](https://crates.io/crates/hdr10plus) crate by quietvoid (MIT).

This software is based in part on the work of the Independent JPEG Group. See [THIRD-PARTY-LICENCES](THIRD-PARTY-LICENCES) for full details.