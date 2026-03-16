# Honey, I Shrunk The Vids

[![Quillx](https://raw.githubusercontent.com/qainsights/Quillx/main/badges/quillx-3.svg)](https://github.com/qainsights/Quillx)

A cross-platform batch video encoder with a GUI. Point it at your files, pick a target bitrate, hit Start. It figures out the rest.

Ships as a desktop GUI app and a headless CLI binary (see [CLI-README.md](CLI-README.md)) built from the same encoding engine. Both are available as portable binaries from the [Releases page](https://github.com/obelisk-complex/histv-universal/releases).

---

This project was built with the assistance of [claude.ai](https://claude.ai). If you find an issue or an inefficiency I've missed, please open an issue or leave a comment.

---

### Screenshots
<details>

![](https://media.piefed.ca/posts/Fk/Ba/FkBaiUnsD0GmZmY.png)

![](https://media.piefed.ca/posts/Qf/a2/Qfa2WQy6ieE5Nic.png)

![](https://media.piefed.ca/posts/FA/a6/FAa6w1zwMHqEeKj.png)

![](https://media.piefed.ca/posts/9Q/B7/9QB78eFzN3sozTZ.png)

</details>

## Download

Grab the latest build from the **[Releases page](https://github.com/obelisk-complex/histv-universal/releases)**.

| Platform | GUI | CLI |
|----------|-----|-----|
| **Windows** | `histv-windows.exe` | `histv-cli-windows.exe` |
| **Linux** | `.AppImage` | `histv-cli-linux` |
| **macOS (Apple Silicon)** | `.dmg` (arm64) | `histv-cli-macos-arm64` |
| **macOS (Intel)** | `.dmg` (x64) | `histv-cli-macos-x64` |

The GUI is portable - no installation needed. On Linux, mark the AppImage executable (`chmod +x`). On macOS, open the DMG and drag to Applications.

**ffmpeg is required.** The GUI offers to download it automatically on first launch. The CLI expects it on your PATH (`apt install ffmpeg`, `brew install ffmpeg`, `choco install ffmpeg`, etc.).

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

A single target bitrate drives the entire encoding strategy:

| Condition | Action |
|-----------|--------|
| Same codec, at or below target | **Copy** - stream-copy, no re-encode |
| Different codec, at or below target | **CQP/CRF transcode** - quality-based |
| Above target | **VBR transcode** - bitrate-limited, peak = 1.5x target |

After encoding, if the output is larger than the source, the encoder discards it and remuxes the source into the target container instead.

### Who HISTV is for

People who have a collection of video files at various bitrates and codecs and want to compress the ones that need compressing - without babysitting each file, without setting up a server, and without writing scripts. Set the target, add the files, walk away.

### How HISTV compares to other tools
<details>

**vs. HandBrake** - HandBrake expects per-file control. HISTV decides per-file whether to re-encode, stream-copy, or quality-transcode based on each file's existing bitrate and codec. If a file is already below your target in the same codec, it gets copied untouched. HISTV also probes your GPU at startup and auto-falls back to software encoding if hardware fails mid-file. If you want to tweak every setting on every file, HandBrake is the better tool.

**vs. Tdarr** - Tdarr is a media server tool: always-running, plugin-based, multi-node. HISTV is a desktop app you open, use, and close. No database, no Docker, no web dashboard. HISTV's encoding logic is built-in rather than plugin-driven.

**vs. a plain ffmpeg command** - HISTV wraps the decision-making and queue management so you don't have to script it yourself: probing, encoder selection, fallback, overwrite/delete prompts, pause/resume, and post-batch actions.

</details>

## Features

<details>
<summary><strong>Full feature list</strong></summary>

- **Queue management** - drag-and-drop, clipboard paste (Ctrl+V), file/folder picker. Click, Shift+click, Ctrl+click, Ctrl+A, Delete. Right-click context menu for re-queue, remove, clear, open, and reveal actions. Drag to reorder.
- **Smart bitrate decisions** - the Target Bitrate column updates live as you change settings, showing exactly what the app plans to do before you start.
- **Four-tier bitrate probing** - stream headers, MKV tags, format metadata, then full packet counting. Single ffprobe call per file; audio streams cached at probe time.
- **Hardware encoder detection** - test-encodes a short clip with each available GPU encoder at startup. Only verified encoders appear in the dropdown.
- **Hardware fallback** - if a GPU encode fails mid-file, offers to retry with the software encoder and continue the batch.
- **QP / CRF rate control** - software encoders offer QP or CRF. Hardware encoders use QP only.
- **Per-stream audio handling** - each audio stream evaluated individually. Streams already below the cap in the target codec are copied. Unknown/proprietary streams are passed through.
- **HDR awareness** - auto-detects HDR via colour metadata, switches to 10-bit. Untick to tonemap to SDR.
- **Subtitle passthrough** - all subtitle streams mapped and copied.
- **Output modes** - output folder, beside source, or replace source. MKV or MP4 container.
- **Batch controls** - start, pause/resume, cancel current, cancel all. Per-file and overall progress bars. Queue rows show progress fill during encoding.
- **Dry run** - probes every file and populates the plan without encoding.
- **Post-batch actions** - shutdown, sleep, log out, or custom command with cancellable countdown.
- **Log console** - collapsible split panel with real-time ffmpeg output. Optional log file.
- **System notifications** - OS-level notification on batch completion.
- **Auto-clear** - optionally remove completed items when the batch finishes.
- **Themeable** - ships with dark and light themes. Drop custom JSON files into the themes folder. See [THEMES.md](THEMES.md).
- **Built-in ffmpeg downloader** - offers to download ffmpeg automatically if not found.
- **Persistent settings** - saved to JSON config, restored on next launch.

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
