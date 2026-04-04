# histv-cli

Command-line batch video encoder - the headless companion to [Honey, I Shrunk The Vids](https://github.com/obelisk-complex/histv-universal).

Same encoding engine as the desktop app: figures out what each file needs, picks the right codec and container per-file, preserves Dolby Vision and HDR10+ dynamic metadata, uses your GPU if available, handles audio tracks individually, stages remote files in waves before encoding, watches disk space, and checks output sizes. Built for servers and automation.

In TTY mode, the CLI shows a persistent queue table during encoding - completed files, the current file with progress, and upcoming files - matching the GUI's columns (file, sizes, resolution, HDR, bitrates, status). Verbose and quiet modes use traditional line-by-line output.

#### Can I use this with Sonarr/Radarr?

Yes! Please see the [Sonarr-Radarr Guide](https://github.com/obelisk-complex/histv-universal/Sonarr-Radarr-Integration.md) for details.

## Installation

Download the binary from the [Releases page](https://github.com/obelisk-complex/histv-universal/releases). Place it on your PATH. On Linux/macOS: `chmod +x histv-cli-*`

**Prerequisites:** ffmpeg and ffprobe must be on your PATH. The CLI does not download ffmpeg - install it via your package manager. For Dolby Vision preservation, MP4Box (GPAC) is also required on your PATH (`apt install gpac`, `brew install gpac`, etc.). Without it, DV files fall back to HDR10. HDR10+ preservation works without any extra tools.

<details>
<summary><strong>Building from source</strong></summary>

```bash
cargo build --manifest-path src-tauri/Cargo.toml --release --bin histv-cli --no-default-features --features cli
```

No dependency on Tauri, GTK, WebKit, or any GUI framework.

</details>

## Quick start

```bash
# Encode a folder with defaults (auto codec, 4Mbps)
histv-cli /path/to/videos/

# See what it would do without encoding anything
histv-cli --dry-run /path/to/videos/

# Custom settings
histv-cli --codec hevc --bitrate 3 --output /encoded/ /path/to/videos/

# Multiple inputs
histv-cli --bitrate 3 file1.mkv file2.mp4 /folder/

# Best quality mode (adapts to your system's RAM)
histv-cli --precision --bitrate 3 /path/to/videos/

# Maximum device compatibility (H.264/MP4/AC3)
histv-cli --compat /path/to/videos/

# Keep AV1 sources as AV1
histv-cli --preserve-av1 /path/to/videos/

# From a saved job file
histv-cli --job batch.json

# Unattended (scripts / cron)
histv-cli --overwrite skip --fallback yes /path/to/videos/ 2>&1 | tee encode.log
```

## What it does with each file

The target codec is resolved per-file from the source:

| Source codec | Target codec |
|-------------|-------------|
| H.264 | H.264 |
| HEVC | HEVC |
| AV1 (with `--preserve-av1`) | AV1 |
| AV1 (without `--preserve-av1`) | HEVC |
| Anything else (MPEG-2, VP9, etc.) | HEVC |
| With `--compat` | H.264 (all sources) |

Then the encoding decision:

| Situation | What happens |
|-----------|-------------|
| Already small enough, same codec | **Copied as-is** - no re-encoding |
| Already small enough, different codec | **Re-encoded for quality** using QP or CRF |
| Too big | **Shrunk** to hit the target bitrate |
| GIF, APNG, or animated WebP | **Always re-encoded** into a proper video |

If an encode makes the file meaningfully bigger than it was (accounting for container overhead based on frame rate and audio streams), the CLI throws away the encode and copies the original into the new container instead.

### Dolby Vision and HDR10+

When `--hdr` is active (the default for HDR sources), the CLI automatically preserves Dolby Vision and HDR10+ dynamic metadata through re-encoding:

| Source | Tools needed | Result |
|--------|-------------|--------|
| Dolby Vision (any profile) | MP4Box on PATH | Full DV preservation. Output forced to MP4. |
| HDR10+ | None | Full dynamic metadata preservation. |
| DV without MP4Box | None | Falls back to HDR10 base layer. Warning logged. |
| HDR10 / HLG | None | Preserved as-is. |

The dry-run table shows each file's source size, estimated output size, resolution, HDR type (DV8, HDR10+, HDR10, HLG, SDR), source bitrate, and target bitrate. Warnings are logged before encoding starts if any files won't get their best-possible treatment.

## Options

<details>
<summary><strong>Input</strong></summary>

| Flag | Description |
|------|-------------|
| `[INPUTS]...` | Files and/or folders to encode |
| `-j, --job <FILE>` | Load settings from a JSON job file |
| `--export-job <FILE>` | Save current flags + inputs to a job file, then exit |

</details>

<details>
<summary><strong>Video</strong></summary>

| Flag | Default | Description |
|------|---------|-------------|
| `-c, --codec <CODEC>` | `auto` | `auto` (preserve source family), `hevc`, or `h264` |
| `-e, --encoder <n>` | auto | Force a specific encoder (e.g. `hevc_nvenc`, `libx265`, `libsvtav1`) |
| `-b, --bitrate <MBPS>` | `4` | Target bitrate in Mbps - files above this get shrunk |
| `--peak-multiplier <MULT>` | `1.5` | How much bitrate can spike on complex scenes (1.0-3.0) |
| `--rc <MODE>` | `qp` | Quality mode for below-target files: `qp` (predictable size) or `crf` (better looking) |
| `--qp-i <N>` | `20` | QP quality for key frames, 0–51 (lower = sharper, bigger) |
| `--qp-p <N>` | `22` | QP quality for normal frames, 0–51 (lower = sharper, bigger) |
| `--crf <N>` | `20` | CRF quality level, 0–51 (lower = sharper, bigger). Software encoders only. |
| `--hdr` | auto | Keep HDR as-is |
| `--no-hdr` | | Convert HDR to SDR with proper tonemapping |
| `--compat` | | Force H.264/MP4/AC3 for maximum device compatibility. Conflicts with `--preserve-av1`. |
| `--preserve-av1` | | Keep AV1 sources as AV1 instead of converting to HEVC. Conflicts with `--compat`. |

</details>

<details>
<summary><strong>Audio</strong></summary>

| Flag | Default | Description |
|------|---------|-------------|
| `-a, --audio <CODEC>` | `auto` | `auto` (copy below cap, re-encode above), `ac3`, `eac3`, `aac`, `copy` |
| `--audio-cap <KBPS>` | `640` | Audio tracks below this bitrate are copied as-is. Tracks above are re-encoded to the same codec at the cap. Codecs with no ffmpeg encoder (DTS, TrueHD) fall back to EAC3. |

</details>

<details>
<summary><strong>Output</strong></summary>

| Flag | Default | Description |
|------|---------|-------------|
| `-o, --output <DIR>` | `./output` | Where to save encoded files |
| `--output-mode <MODE>` | `folder` | `folder` (use --output), `beside` (subfolder next to each input), `replace` (swap the original) |
| `--container <FMT>` | `auto` | `auto` (preserve source: MKV/MP4/MOV kept, others become MKV), `mkv`, `mp4` |
| `--overwrite <POLICY>` | `ask` | What to do when output already exists: `ask`, `yes`, `skip` |
| `--delete-source` | | Delete the original after a successful encode |

</details>

<details>
<summary><strong>Performance</strong></summary>

| Flag | Default | Description |
|------|---------|-------------|
| `--precision` | | Best quality mode - tests clips first, adapts strategy to your RAM, caps bitrate. Software CRF only. |
| `--threads <N>` | `0` | Limit CPU threads, 0–64 (0 = use all available) |
| `--low-priority` | | Don't slow down other apps while encoding |

</details>

<details>
<summary><strong>Behaviour</strong></summary>

| Flag | Default | Description |
|------|---------|-------------|
| `--dry-run` | | Show the plan without encoding anything |
| `--repair-tags` | | Fix stale MKV stream statistics tags on the input files, then exit. No encoding. |
| `--deep-repair` | | Like `--repair-tags` but scans every packet for exact byte and frame counts. Slower, more accurate. |
| `--fallback <POLICY>` | `ask` | If GPU encoding fails: `ask`, `yes` (auto-retry with software), `no` |
| `--remote <POLICY>` | `auto` | Network files: `auto` (copy locally if needed), `always`, `never` |
| `--local-tmp <DIR>` | system temp | Where to put local copies of network files |
| `--disk-limit <PCT>` | `off` | Pause when disk usage hits this % (50-99). Turns on `--delete-source`. |
| `--disk-resume <PCT>` | baseline | Resume when disk drops below this % |
| `--post-command <CMD>` | | Run a command when the batch finishes |
| `--save-log` | | Save a log file to the output folder |
| `--log-level <LEVEL>` | `normal` | `quiet`, `normal`, `verbose` |

</details>

## Precision mode

`--precision` gives you the best quality the encoder can produce. It picks its strategy based on your system's RAM:

| Your RAM | What it does |
|----------|-------------|
| 16GB or more | Looks 250 frames ahead to plan bitrate (single pass) |
| 8-16GB | Looks 120 frames ahead to plan bitrate (single pass) |
| Under 8GB | Scans the whole file first, then encodes (two passes) |

Before starting the full encode, it tests three 10-second clips from different parts of the file. If the estimated output would be bigger than the original, it switches to a safer mode automatically. Files under 2 minutes skip this check.

Output bitrate is capped based on your `--bitrate` and `--peak-multiplier` settings.

## Job files

<details>
<summary><strong>Format and example</strong></summary>

A JSON file with the same settings as CLI flags, plus a `files` array. All fields are optional - anything you leave out uses the default. CLI flags override job file values.

```json
{
  "files": ["/srv/media/movies/", "/srv/media/clips/holiday.mkv"],
  "codec": "auto",
  "bitrate": 4,
  "peakMultiplier": 1.5,
  "rateControl": "qp",
  "qpI": 20,
  "qpP": 22,
  "audioCodec": "auto",
  "audioBitrateCap": 640,
  "output": "/srv/media/encoded/",
  "outputMode": "folder",
  "container": "auto",
  "overwrite": "ask",
  "deleteSource": false,
  "compatibilityMode": false,
  "preserveAv1": false,
  "fallback": "ask",
  "remote": "auto",
  "precisionMode": false,
  "threads": 0,
  "lowPriority": false,
  "saveLog": true
}
```

Generate a starter job file from your current flags:

```bash
histv-cli --export-job my-batch.json --codec hevc --bitrate 8 /srv/media/
```

</details>

## Network drive handling

<details>
<summary><strong>Details</strong></summary>

When your files are on a network share (NFS, SMB, sshfs, etc.), the CLI stages them locally in waves before encoding so the network doesn't bottleneck the encoder. The wave planner groups remote files to fit available staging space, stages an entire wave at once, encodes the wave, cleans up, and repeats. Local files between waves are encoded in place.

| `--remote` value | Files on a network drive | Files on local disk |
|------------------|-------------------------|---------------------|
| `auto` (default) | Staged locally in waves | Encoded in place |
| `always` | Staged locally in waves | Staged locally in waves |
| `never` | Encoded in place | Encoded in place |

Detection is automatic on all platforms. The temp folder defaults to your system temp directory. Override with `--local-tmp`. The dry-run output shows the wave staging plan when remote files are present.

</details>

## Signal handling

| Signal | What happens |
|--------|-------------|
| Ctrl+C (once) | Stops the current file, moves to the next |
| Ctrl+C (twice quickly) | Stops everything |

Partial output files and temp copies are cleaned up automatically.

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | Everything worked |
| 1 | Some files failed |
| 2 | Cancelled |
| 3 | Nothing to encode |
| 4 | Something went wrong before encoding could start (ffmpeg missing, can't write to output, etc.) |

## Unattended usage

When output is piped or redirected (not a terminal):

- Overwrite prompts default to **skip**
- GPU fallback prompts default to **yes** (auto-retry with software)
- No colours or progress bars

```bash
histv-cli --overwrite skip --fallback yes --log-level quiet /path/to/videos/
```

## Disk space monitoring

`--disk-limit` pauses encoding when your output drive gets too full, and resumes when space frees up. Automatically turns on `--delete-source` (otherwise the drive would never free up).

```bash
histv-cli --disk-limit 90 /path/to/videos/
```

`--dry-run` includes a disk space estimate showing how much space the batch will need.