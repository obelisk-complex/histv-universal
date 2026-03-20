# histv-cli

Command-line batch video encoder - the headless companion to [Honey, I Shrunk The Vids](https://github.com/obelisk-complex/histv-universal).

Same encoding engine as the desktop app: figures out what each file needs, uses your GPU if available, handles audio tracks individually, copies files from network drives before encoding, watches disk space, and checks output sizes. Built for servers and automation.

## Installation

Download the binary from the [Releases page](https://github.com/obelisk-complex/histv-universal/releases). Place it on your PATH. On Linux/macOS: `chmod +x histv-cli-*`

**Prerequisite:** ffmpeg and ffprobe must be on your PATH. The CLI does not download ffmpeg - install it via your package manager.

<details>
<summary><strong>Building from source</strong></summary>

```bash
cargo build --manifest-path src-tauri/Cargo.toml --release --bin histv-cli --no-default-features --features cli
```

No dependency on Tauri, GTK, WebKit, or any GUI framework.

</details>

## Quick start

```bash
# Encode a folder with defaults (HEVC, 5Mbps)
histv-cli /path/to/videos/

# See what it would do without encoding anything
histv-cli --dry-run /path/to/videos/

# Custom settings
histv-cli --codec hevc --bitrate 3 --output /encoded/ /path/to/videos/

# Multiple inputs
histv-cli --bitrate 3 file1.mkv file2.mp4 /folder/

# Best quality mode (adapts to your system's RAM)
histv-cli --precision --bitrate 3 /path/to/videos/

# From a saved job file
histv-cli --job batch.json

# Unattended (scripts / cron)
histv-cli --overwrite skip --fallback yes /path/to/videos/ 2>&1 | tee encode.log
```

## What it does with each file

| Situation | What happens |
|-----------|-------------|
| Already small enough, same codec | **Copied as-is** - no re-encoding |
| Already small enough, different codec | **Re-encoded for quality** using QP or CRF |
| Too big | **Shrunk** to hit the target bitrate |
| GIF or APNG | **Always re-encoded** into a proper video |

If an encode makes the file bigger than it was, the CLI throws away the encode and copies the original into the new container instead.

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
| `-c, --codec <CODEC>` | `hevc` | `hevc` (smaller files) or `h264` (more compatible) |
| `-e, --encoder <n>` | auto | Force a specific encoder (e.g. `hevc_nvenc`, `libx265`) |
| `-b, --bitrate <MBPS>` | `5` | Target bitrate in Mbps - files above this get shrunk |
| `--peak-multiplier <MULT>` | `1.5` | How much bitrate can spike on complex scenes (1.0-3.0) |
| `--rc <MODE>` | `qp` | Quality mode for below-target files: `qp` (predictable size) or `crf` (better looking) |
| `--qp-i <N>` | `20` | QP quality for key frames (lower = sharper, bigger) |
| `--qp-p <N>` | `22` | QP quality for normal frames (lower = sharper, bigger) |
| `--crf <N>` | `20` | CRF quality level (lower = sharper, bigger). Software encoders only. |
| `--hdr` | auto | Keep HDR as-is |
| `--no-hdr` | | Convert HDR to SDR with proper tonemapping |

</details>

<details>
<summary><strong>Audio</strong></summary>

| Flag | Default | Description |
|------|---------|-------------|
| `-a, --audio <CODEC>` | `ac3` | Audio format: `ac3` (universal), `eac3` (smaller), `aac` (smallest), `copy` (leave alone) |
| `--audio-cap <KBPS>` | `640` | Audio tracks already below this bitrate are left alone |

</details>

<details>
<summary><strong>Output</strong></summary>

| Flag | Default | Description |
|------|---------|-------------|
| `-o, --output <DIR>` | `./output` | Where to save encoded files |
| `--output-mode <MODE>` | `folder` | `folder` (use --output), `beside` (subfolder next to each input), `replace` (swap the original) |
| `--container <FMT>` | `mkv` | `mkv` (holds everything) or `mp4` (plays on more devices) |
| `--overwrite <POLICY>` | `ask` | What to do when output already exists: `ask`, `yes`, `skip` |
| `--delete-source` | | Delete the original after a successful encode |

</details>

<details>
<summary><strong>Performance</strong></summary>

| Flag | Default | Description |
|------|---------|-------------|
| `--precision` | | Best quality mode - tests clips first, adapts strategy to your RAM, caps bitrate. Software CRF only. |
| `--threads <N>` | `0` | Limit CPU threads (0 = use all available) |
| `--low-priority` | | Don't slow down other apps while encoding |

</details>

<details>
<summary><strong>Behaviour</strong></summary>

| Flag | Default | Description |
|------|---------|-------------|
| `--dry-run` | | Show the plan without encoding anything |
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
  "codec": "hevc",
  "bitrate": 5,
  "peakMultiplier": 1.5,
  "rateControl": "qp",
  "qpI": 20,
  "qpP": 22,
  "audioCodec": "ac3",
  "audioBitrateCap": 640,
  "output": "/srv/media/encoded/",
  "outputMode": "folder",
  "container": "mkv",
  "overwrite": "ask",
  "deleteSource": false,
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
histv-cli --export-job my-batch.json --codec h264 --bitrate 8 /srv/media/
```

</details>

## Network drive handling

<details>
<summary><strong>Details</strong></summary>

When your files are on a network share (NFS, SMB, sshfs, etc.), the CLI can copy them to local storage before encoding so the network doesn't slow things down.

| `--remote` value | Files on a network drive | Files on local disk |
|------------------|-------------------------|---------------------|
| `auto` (default) | Copied locally first | Encoded in place |
| `always` | Copied locally first | Copied locally first |
| `never` | Encoded in place | Encoded in place |

Detection is automatic on all platforms. The temp folder defaults to your system temp directory. Override with `--local-tmp`.

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