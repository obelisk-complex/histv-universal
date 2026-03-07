# Honey, I Shrunk The Vids [Mr. Universe Edition]

A cross-platform batch video encoder with a GUI. Point it at your files, pick a target bitrate, hit Start. It figures out the rest.

-----
This project was created with the assistance of https://claude.ai, but don't let that stop you from checking it out. This is a learning experience for me, so I'm not just slapping things together with an AI model and poking it until it does what I need it to on my system, then calling it done. I'm actively trying to understand the output, modify and fix it myself where I can, and eliminate redundancies and bloat and other issues.

My hardware for testing is limited, and while this is a learning project for me, one thing I do not need any practice with is test case design. I've done it professionally, I'm good at it, I hate it because it's so repetetive. If I were building commercial software I would write and run test cases, but it's not, so I'm not doing anything more than informal testing to make sure my updates generally work. Otherwise, I'll end up hating my hobbies.

If you find an issue and you'd like me to fix it, or if you find an inefficiency that I've missed cleaning up, please by all means open an issue or leave a comment.

Thanks for reading, I hope you find HISTV useful! 

---

<details>
   <summary>Screenshots</summary>
  
![](https://media.piefed.ca/posts/Fk/Ba/FkBaiUnsD0GmZmY.png)  

![](https://media.piefed.ca/posts/Qf/a2/Qfa2WQy6ieE5Nic.png)  

![](https://media.piefed.ca/posts/FA/a6/FAa6w1zwMHqEeKj.png)  

![](https://media.piefed.ca/posts/9Q/B7/9QB78eFzN3sozTZ.png)  

</details>

### Why this exists

I was doing a lot of manual re-encoding down from insane source bitrates with ffmpeg, and I started wondering if I could put my PowerShell script into a nice GUI. Then I wondered if I could give it a dark theme... and a file queue... and on and on and on... until finally I had it working how I wanted. Then I wondered, because Windows is awful, if I could make it platform-agnostic. And here we are. It's got a dark theme because of course and a light theme because I guess, also it's themeable because why the hell not (see [THEMES.md](THEMES.md)).

The core idea hasn't changed: point it at a file or folder, let it enumerate the multimedia files, pick a target bitrate, hit Start. You don't even have to pick an output folder; by default outputs go into /output in the same folder as the application is running from.


## How HISTV compares to other tools

There are already several ways to batch-encode video. Here is why HISTV exists alongside them and who it is for.

### vs. HandBrake

HandBrake is excellent when you want fine-grained, per-file control - tweaking RF values, cropping, adding filters, adjusting chapter markers. It expects you to care about each file individually, even in queue mode. HISTV takes the opposite approach: you set a single target bitrate once, drop in hundreds of files, and the app decides per-file whether to re-encode, stream-copy, or quality-transcode based on each file's existing bitrate and codec. If a file is already below your target in the same codec, it gets copied untouched - no wasted time, no generation loss. HandBrake does not make that decision for you.

HISTV also probes your GPU at startup and builds the encoder list from what actually works on your hardware, rather than presenting every possible option and leaving you to discover at encode time that your system does not support it. If a hardware encode fails mid-file, HISTV offers to retry with the software encoder automatically - HandBrake simply fails.

If you want to tweak every setting on every file, HandBrake is the better tool. If you have a pile of home videos at wildly different bitrates and just want to shrink the ones that need shrinking, HISTV is more efficient.

### vs. Tdarr

Tdarr is designed for media servers - it watches folders, applies plugin-based rules, and distributes work across nodes. It is powerful but complex to set up, requires a server architecture, and assumes you want an always-running service. HISTV is a desktop app. You open it, add files, hit Start, and close it when you are done. There is no database, no node configuration, no Docker containers, no web dashboard to manage.

Tdarr also relies on community plugins for its encoding logic, which means the behaviour depends on which plugins you install and how you configure their priority. HISTV has the logic built in: above-target files get VBR-encoded, below-target same-codec files get stream-copied, below-target cross-codec files get quality-transcoded. That is the entire decision tree, and it is visible in the queue's Target Bitrate column before you start.

If you run a Plex or Jellyfin library and want continuous, automated processing across multiple machines, Tdarr is purpose-built for that. If you want to compress a batch of files on your own PC without setting up infrastructure, HISTV is simpler.

### vs. a plain ffmpeg command

You can absolutely do everything HISTV does with raw ffmpeg commands. HISTV does not add any encoding capability that ffmpeg does not already have - it just wraps the decision-making and queue management so you do not have to script it yourself. Specifically, HISTV handles: probing every file's bitrate and codec to decide the right encoding strategy, selecting and verifying GPU encoders, falling back to software when hardware fails, managing overwrite and delete-source prompts across a batch, pausing and resuming mid-batch, and shutting down your machine when the batch finishes at 3am.

If you are comfortable writing shell scripts and already have your own ffmpeg wrapper, HISTV probably does not offer much. If you find yourself repeatedly writing the same ffmpeg one-liners and wishing you had a queue with pause/resume and a progress bar, HISTV saves that effort.

### Who HISTV is for

HISTV is for people who have a collection of video files at various bitrates and codecs and want to compress the ones that need compressing - without babysitting each file, without setting up a server, and without writing scripts. It is a "set the target, add the files, walk away" tool.


## What it does

**Queue management** - drag-and-drop files or folders onto the queue, paste paths from the clipboard with Ctrl+V, or use the file/folder picker buttons. Select rows with click, Shift+click for range selection, Ctrl+click for multi-selection, Ctrl+A to select all, and Delete to remove selected items. Right-click for a context menu with options to re-queue, remove, clear completed/non-pending items, open the source file, or reveal it in your file manager.

**Smart bitrate decisions** - a single target bitrate drives the entire encoding strategy. Files above the target are VBR-encoded down to that bitrate (with a 1.5x peak cap). Files already at or below the target in the same codec are stream-copied untouched - no re-encoding, no generation loss. Files at or below the target but in a different codec are quality-transcoded using QP or CRF mode. The Target Bitrate column in the queue updates live as you change settings, so you can see exactly what the app plans to do before you start.

**Four-tier bitrate probing** - reads stream-level bitrate headers first, then MKV container tags, then format-level metadata, and falls back to full packet counting for files that have no bitrate metadata at all. All of this happens in a single ffprobe call per file (only the rare packet-counting fallback needs a second call), with audio stream data cached at probe time so no additional probing is needed during encoding.

**Hardware encoder detection** - on startup, the app test-encodes a short clip with each GPU encoder available on your platform (AMF, NVENC, QSV, VideoToolbox, VAAPI). The encoder dropdown only shows the ones that actually succeeded, so you never pick an encoder your hardware cannot run. Software encoders (libx265, libx264) are always available as a fallback.

**Hardware fallback** - if a GPU encode fails mid-file, a prompt offers to retry that file with the software encoder and continue the batch. You do not lose progress on the rest of the queue.

**QP / CRF rate control** - software encoders (libx265, libx264) offer a choice between QP (fixed quantiser per frame) and CRF (perceptual quality target). A segmented toggle appears when a software encoder is selected. Hardware encoders use QP only, since CRF is not supported by their APIs.

**Per-stream audio handling** - each audio stream is evaluated individually. Streams already below the audio cap in the target codec are copied untouched. Everything else is re-encoded to your chosen codec (AC3, EAC3, or AAC) at the specified bitrate cap.

**HDR awareness** - automatically detects HDR sources via colour metadata (transfer characteristics, colour primaries, matrix coefficients) and switches to 10-bit pixel format to preserve HDR. Untick the HDR checkbox to tonemap HDR sources down to SDR instead.

**Subtitle passthrough** - all subtitle streams are mapped and copied through to the output file without re-encoding.

**Output options** - choose MKV or MP4 container format, set a custom output folder, or tick "Put output next to input file" to create an output subfolder alongside each source file. Overwrite and delete-source behaviours can be set to always-on or prompted per file.

**Batch controls** - start, pause/resume, cancel current file, or cancel the entire batch. A progress bar and status line show the current file and encoding command in real time.

**Dry run** - probes every file in the queue and populates the Target Bitrate column without encoding anything, so you can preview the plan.

**Post-batch actions** - schedule a shutdown, sleep, log out, or custom command to run when the batch finishes, with a cancellable countdown timer.

**Log console** - a collapsible drawer at the bottom of the window showing ffmpeg output and app events in real time. Optionally save the log to a timestamped text file in the output directory.

**System notifications** - an OS-level notification when the batch completes, so you know it is done even if the app is in the background.

**Auto-clear** - optionally remove completed items from the queue when the batch finishes, keeping only failed, skipped, and cancelled items.

**Themeable** - ships with dark and light themes. Drop custom JSON theme files into the themes folder and they appear in the dropdown. See [THEMES.md](THEMES.md) for the format.

**Built-in ffmpeg downloader** - if ffmpeg is not found on your system, the app offers to download it automatically with a real-time progress indicator. Downloaded binaries are stored in your platform's app-data directory, not cluttering up your working folders.

**Keyboard shortcuts** - Ctrl+V to paste paths, Ctrl+A to select all, Ctrl+R to re-queue selected, Delete/Backspace to remove selected items.

**Persistent settings** - all settings are saved automatically to a JSON config file in your platform's app-data directory and restored on next launch.

**Differential rendering** - the queue table uses differential DOM updates rather than full rebuilds, and debounced refresh scheduling to stay responsive even with large queues.


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
