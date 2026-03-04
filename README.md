# Honey, I Shrunk The Vids

A cross-platform batch video encoder built with [Tauri v2](https://v2.tauri.app/) and Rust. Designed for re-encoding large media libraries down to sensible bitrates using hardware-accelerated encoders where available, with automatic fallback to software encoding.

## Features

- **Batch queue** — drag-and-drop files or folders, paste paths from the clipboard, or use the file picker
- **Automatic encoder detection** — discovers available GPU encoders (AMF, NVENC, QSV, VideoToolbox, VAAPI) and falls back to libx264/libx265
- **Smart bitrate decisions** — files above the target bitrate are re-encoded with VBR; files below it in the same codec family are stream-copied; cross-codec files use CQP
- **Per-stream audio handling** — audio streams below the cap in the target codec are copied, otherwise re-encoded to AC3, EAC3 or AAC
- **HDR awareness** — detects HDR sources and preserves 10-bit colour when the HDR option is enabled; can also tonemap HDR to SDR
- **Overwrite / delete-source prompts** with "always" options to avoid repeated confirmation
- **Hardware fallback prompt** — if a GPU encode fails, offers to retry with the software encoder
- **Dry run mode** — probes all queued files without encoding, so you can preview target bitrate decisions
- **Pause / resume / cancel** controls for individual files or the entire batch
- **Post-batch actions** — shutdown, sleep, log out, or run a custom command after encoding finishes, with a cancellable countdown
- **Themeable UI** — ships with dark and light themes; drop additional JSON theme files into the `themes/` folder
- **Log console** — collapsible drawer showing ffmpeg output and application events, with optional log-to-file
- **System notifications** on batch completion (via Tauri's notification plugin)
- **ffmpeg auto-download** — if ffmpeg isn't found on the system, offers to download and extract it automatically (Windows and Linux)

## Supported platforms

| OS      | Architecture     | GPU encoders                     |
|---------|-----------------|----------------------------------|
| Windows | x86_64          | AMF, NVENC, QSV                  |
| Linux   | x86_64          | VAAPI, NVENC, QSV                |
| macOS   | x86_64 / ARM64  | VideoToolbox                     |

## Prerequisites

- **Rust** (stable) — [install via rustup](https://rustup.rs/)
- **Tauri v2 CLI** — `cargo install tauri-cli --version "^2"`
- **ffmpeg and ffprobe** — either on your system PATH, placed in the app's `binaries/` folder, or let the app download them on first run
- Platform-specific Tauri dependencies — see the [Tauri prerequisites guide](https://v2.tauri.app/start/prerequisites/)

## Building

```bash
cargo tauri dev        # run in development mode with hot reload
cargo tauri build      # produce a release installer/bundle
```

## Project structure

```
├── src/                    # Frontend (HTML + CSS + inline JS)
│   ├── index.html
│   └── css/
│       └── app.css
├── src-tauri/
│   ├── Cargo.toml
│   ├── tauri.conf.json
│   ├── build.rs
│   ├── capabilities/
│   ├── src/
│   │   ├── main.rs         # Entry point
│   │   ├── lib.rs          # Tauri commands and app setup
│   │   ├── encoder.rs      # Encoder detection, flag mapping, batch logic
│   │   ├── ffmpeg.rs       # ffmpeg/ffprobe path resolution and download
│   │   ├── probe.rs        # Media file probing via ffprobe
│   │   ├── queue.rs        # Queue data structures and file collection
│   │   ├── config.rs       # Persistent settings (JSON)
│   │   └── themes.rs       # Theme loading
│   └── themes/
│       ├── default-dark.json
│       └── default-light.json
└── README.md
```

## Configuration

Settings are persisted automatically to a JSON file in the platform's app-data directory. All options are accessible from the UI's Encoding Settings and Output Settings tabs.

## Licence

TBD
