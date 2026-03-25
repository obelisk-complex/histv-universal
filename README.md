# Honey, I Shrunk The Vids

[![Quillx](https://raw.githubusercontent.com/qainsights/Quillx/main/badges/quillx-3.svg)](https://github.com/qainsights/Quillx)

A cross-platform batch video encoder that shrinks your video files to a target quality level. No need to babysit the queue or tweak settings per file - just pick a target bitrate, add your files, and hit start.

Available as a desktop app and a headless CLI for servers (see [CLI-README.md](CLI-README.md)). Both use the same encoding engine. Portable binaries are on the GitHub [Releases page](https://github.com/obelisk-complex/histv-universal/releases).

---

### Screenshots
<details>

![](/screenshots/gui-1.png)

![](/screenshots/gui-2.png)

![](/screenshots/gui-3.png)

![](/screenshots/gui-4.png)

![](/screenshots/cli-1.png)

![](/screenshots/cli-2.png)

![](/screenshots/cli-3.png)

</details>

## Features

<details>
<summary><strong>Full feature list</strong></summary>

- **Queue management** - drag-and-drop, clipboard paste (Ctrl+V), file/folder picker. Click, Shift+click, Ctrl+click, Ctrl+A, Delete. Right-click context menu. Drag to reorder.
- **GIF/APNG support** - animated GIFs and APNGs are converted to proper video files automatically.
- **Smart decisions** - the app figures out what to do with each file before you start. Files that are already small enough get copied untouched. Files that are too big get shrunk. The queue shows the plan in real time as you change settings.
- **GPU encoding** - tests your GPU at startup and picks the fastest working encoder. Falls back to software automatically if the GPU encoder fails mid-file.
- **QP / CRF quality modes** - two ways to control quality when files are below your target bitrate. QP gives predictable sizes; CRF gives better-looking results with less predictable sizes.
- **Precision mode** - one checkbox for the best possible quality. Uses software encoding with smart analysis that adapts to your system's RAM. Tests a few short clips first to make sure it won't make the file bigger. Slower, but produces the smallest file that still looks great.
- **VBR peak ceiling** - controls how much the bitrate can spike on action-heavy scenes (1.5x to 3x). Higher values look better on complex content.
- **Audio handling** - each audio track is handled separately. Tracks already small enough are left alone. Unrecognised audio formats (like Apple Spatial Audio) are skipped with a warning instead of crashing.
- **HDR support** - HDR videos are detected automatically and encoded in 10-bit. Untick the HDR checkbox to convert HDR to SDR with industry-standard tonemapping so colours look right on a normal screen.
- **Subtitles** - all subtitle tracks are kept.
- **Output options** - save to a folder, next to the source file, or replace the source. MKV or MP4 output.
- **Size safety net** - if the encoded file ends up bigger than the original, the app throws it away and copies the original into the new container instead.
- **Performance controls** - limit CPU threads and/or run encoding at low priority so your PC stays usable.
- **ETA** - shows estimated time remaining in the progress bar and the window title, so you can see it from the taskbar.
- **Batch controls** - start, pause, cancel current file, cancel everything. Progress bars per file and overall.
- **Dry run** - shows what the app would do without actually encoding anything.
- **Network drive support** - detects files on network shares and copies them locally before encoding, so slow networks don't bottleneck the encoder.
- **Disk space monitoring** - pauses encoding if your drive gets too full, resumes when space frees up.
- **Post-batch actions** - shut down, sleep, log out, or run a custom command when the batch finishes.
- **Log console** - colour-coded log with filters, optional file export.
- **Notifications** - system notification when the batch finishes.
- **Auto-clear** - optionally clears finished items from the queue.
- **Themes** - dark and light themes included. Drop custom JSON theme files into the themes folder. See [THEMES.md](THEMES.md).
- **ffmpeg downloader** - offers to download ffmpeg for you if it's not installed.
- **Remembers settings** - everything is saved to config.json in the same folder as the .exe, and restored on next launch.
- **CLI version** - same engine as a command-line tool for servers and scripts. See [CLI-README.md](CLI-README.md).

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

All binaries are portable - no installation needed. On Linux, mark the AppImage executable (`chmod +x`). On macOS, open the DMG and drag to Applications. On Windows, extract the zip to a folder.

If you use a standard build, ffmpeg is required. The GUI offers to download it on first launch. The CLI expects it on your PATH (`apt install ffmpeg`, `brew install ffmpeg`, `choco install ffmpeg`, etc.).

| OS | Architecture | GPU encoders |
|----|-------------|-------------|
| Windows | x86_64 | AMF, NVENC, QSV |
| Linux | x86_64 | VAAPI, NVENC, QSV |
| macOS | x86_64 / ARM64 | VideoToolbox |

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
| Already small enough, same codec | **Copied as-is** - no re-encoding |
| Already small enough, different codec | **Re-encoded for quality** using QP or CRF |
| Too big | **Shrunk** to hit the target bitrate |
| GIF or APNG | **Always re-encoded** into a proper video |

If an encode makes the file bigger than it was, the app throws away the encode and copies the original into the new container instead.

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
├── src/                        # Frontend (HTML + CSS + inline JS)
│   ├── index.html
│   └── css/app.css
├── src-tauri/
│   ├── Cargo.toml
│   ├── tauri.conf.json
│   ├── build.rs
│   ├── src/
│   │   ├── main.rs             # GUI entry point
│   │   ├── cli_main.rs         # CLI entry point
│   │   ├── lib.rs              # Tauri commands, app setup, shared state
│   │   ├── encoder.rs          # Encoder detection, flag mapping, batch loop
│   │   ├── ffmpeg.rs           # Binary resolution, download, spawning
│   │   ├── probe.rs            # Media probing via ffprobe
│   │   ├── queue.rs            # Queue data structures, file collection
│   │   ├── events.rs           # EventSink + BatchControl trait definitions
│   │   ├── tauri_sink.rs       # GUI EventSink (Tauri emit)
│   │   ├── tauri_batch_control.rs # GUI BatchControl (Mutex + emit-and-poll)
│   │   ├── cli_sink.rs         # CLI EventSink (stderr + indicatif)
│   │   ├── batch_control.rs    # CLI BatchControl (atomics + TTY prompts)
│   │   ├── cli.rs              # CLI arg parsing (clap) + job files
│   │   ├── remote.rs           # Network mount detection + caching
│   │   ├── staging.rs          # Local staging for remote files
│   │   ├── disk_monitor.rs     # Disk-space estimation + runtime monitoring
│   │   ├── config.rs           # GUI persistent settings
│   │   └── themes.rs           # Theme loading
│   └── themes/
│       ├── default-dark.json
│       └── default-light.json
├── .github/workflows/          # CI: per-platform builds
└── README.md
```

</details>

### Licence

[Do whatever you want.](LICENSE)

### Third-party software

The **-full** builds bundle [FFmpeg](https://ffmpeg.org), which is licensed under the [GNU General Public Licence v2+](https://www.gnu.org/licenses/old-licenses/gpl-2.0.html). The bundled binaries are unmodified static GPL builds from [BtbN/FFmpeg-Builds](https://github.com/BtbN/FFmpeg-Builds) (Windows/Linux) and [evermeet.cx](https://evermeet.cx/ffmpeg/) (macOS). The corresponding source code is available from those repositories. We will provide the source on request for three years from the date of each release - file an issue on this repo. This software is based in part on the work of the Independent JPEG Group. See [THIRD-PARTY-LICENCES](THIRD-PARTY-LICENCES) for full details.