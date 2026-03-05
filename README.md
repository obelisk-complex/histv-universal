# Honey, I Shrunk The Vids [Mr. Universe Edition]

### Why this exists

I was doing a lot of manual re-encoding down from insane source bitrates with FFMPEG, and I started wondering if I could put my Powershell script into a nice GUI. Then I wondered if I could give it a dark theme... and a file queue... and on and on and on... until finally I had it working how I wanted. Then I wondered, because Windows is awful, if I could make it platform-agnostic. And here we are. It's got a dark theme because of course and a light theme because I guess, also it's themeable because why the hell not (see [THEMES.md](https://github.com/obelisk-complex/histv-universal/blob/main/THEMES.md))

The core idea hasn't changed: point it at a file or folder, let it enumerate the multimedia files, pick a target bitrate, hit Start. You don't even have to pick an output folder; by default outputs go into /output in the same folder as the application is running from.

<details>
   <summary>Screenshots</summary>
  
![](https://media.piefed.ca/posts/Zh/5q/Zh5qETVk8VWnkks.png)  

![](https://media.piefed.ca/posts/R4/aC/R4aCX6NcjH5tjqU.png)  

![](https://media.piefed.ca/posts/Ok/Eo/OkEohnm6jMVh1Ei.png)  

![](https://media.piefed.ca/posts/D4/g3/D4g317xAHJTh9Uq.png)  

![](https://media.piefed.ca/posts/rH/p5/rHp5X4I5v7CMw6Q.png)  

</details>

### What it does

- **Batch queue** - drag-and-drop files or folders, paste paths from the clipboard, or use the file picker
- **Hardware encoder detection** - tests each GPU encoder (AMF, NVENC, QSV, VideoToolbox, VAAPI) with a real encode at startup, so the dropdown only shows what actually works on your hardware. Software encoding (libx265/libx264) is always available as a fallback
- **Smart bitrate decisions** - files above your target bitrate get re-encoded with VBR; files already below it in the same codec are stream-copied untouched; cross-codec files use constant quality (CQP)
- **Four-tier bitrate probing** - reads stream headers, MKV tags, container metadata, or falls back to full packet counting so it can make the right decision for any file format
- **Per-stream audio handling** - audio streams already below the cap in the right codec are copied; everything else is re-encoded to AC3, EAC3, or AAC at your chosen bitrate
- **HDR awareness** - detects HDR sources and preserves 10-bit colour, or tonemaps to SDR if you prefer
- **Overwrite / delete-source prompts** with "always" options so you're not clicking through dialogs all night
- **Hardware fallback** - if a GPU encode fails mid-file, offers to retry with the software encoder
- **Dry run** - probes everything without encoding, so you can preview what it plans to do
- **Pause / resume / cancel** - per-file or for the whole batch
- **Post-batch actions** - shutdown, sleep, log out, or run a custom command when the batch finishes, with a cancellable countdown
- **Themeable** - dark and light themes included; drop custom JSON themes into the themes folder
- **Log console** - collapsible panel showing ffmpeg output and app events in real time, with optional log-to-file
- **System notifications** on batch completion
- **Built-in ffmpeg downloader** - downloads ffmpeg with a progress indicator and stores it in your app data directory, not cluttering up your working folders

## Download

Grab the latest build for your system from the **[Releases page](https://github.com/obelisk-complex/histv-universal/releases)**.

| Platform | What to download | How to run |
|----------|-----------------|------------|
| **Windows** | `histv-windows.exe` | Just run it - no installation needed |
| **Linux** | `.AppImage` | Mark it executable (`chmod +x`) and run it |
| **macOS (Apple Silicon)** | `.dmg` (arm64) | Open the DMG and drag to Applications |
| **macOS (Intel)** | `.dmg` (x64) | Open the DMG and drag to Applications |

**ffmpeg is required** but you don't need to install it yourself. On first launch, if ffmpeg isn't found, the app will offer to download it automatically. On macOS you can also install it via `brew install ffmpeg`.

### Supported platforms

| OS | Architecture | GPU encoders |
|----|-------------|-------------|
| Windows | x86_64 | AMF, NVENC, QSV |
| Linux | x86_64 | VAAPI, NVENC, QSV |
| macOS | x86_64 / ARM64 | VideoToolbox |

### Building from source

You'll need Rust (stable) and the Tauri v2 CLI. See the [Tauri prerequisites guide](https://v2.tauri.app/start/prerequisites/) for platform-specific dependencies.

```bash
cargo install tauri-cli --version "^2"
cargo tauri dev          # development mode
cargo tauri build        # release build
```

### Project structure

```
├── src/                        # Frontend (HTML + CSS + inline JS)
│   ├── index.html
│   └── css/
│       └── app.css
├── src-tauri/
│   ├── Cargo.toml
│   ├── tauri.conf.json
│   ├── build.rs
│   ├── capabilities/
│   ├── src/
│   │   ├── main.rs             # Entry point
│   │   ├── lib.rs              # Tauri commands and app setup
│   │   ├── encoder.rs          # Encoder detection, flag mapping, batch logic
│   │   ├── ffmpeg.rs           # ffmpeg/ffprobe path resolution and download
│   │   ├── probe.rs            # Media file probing via ffprobe
│   │   ├── queue.rs            # Queue data structures and file collection
│   │   ├── config.rs           # Persistent settings (JSON)
│   │   └── themes.rs           # Theme loading
│   └── themes/
│       ├── default-dark.json
│       └── default-light.json
├── .github/
│   └── workflows/
│       └── build.yml           # CI: cross-platform builds and releases
└── README.md
```

### Configuration

Settings are saved automatically to a JSON file in your platform's app-data directory. Everything is accessible from the Encoding Settings and Output Settings tabs in the UI.

### Licence

lol do whatever man
