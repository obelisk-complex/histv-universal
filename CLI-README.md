# histv-cli

Headless batch video encoder - the command-line companion to [Honey, I Shrunk The Vids](https://github.com/obelisk-complex/histv-universal).

Uses the same encoding engine as the desktop app: smart bitrate decisions, hardware-accelerated encoding, per-stream audio handling, and post-encode size checks. Designed for remote and headless servers where a desktop environment is unavailable.

## Installation

Download the binary for your platform from the [Releases](https://github.com/obelisk-complex/histv-universal/releases) page:

| Platform | Binary |
|----------|--------|
| Windows (x86_64) | `histv-cli-windows.exe` |
| Linux (x86_64) | `histv-cli-linux` |
| macOS (Apple Silicon) | `histv-cli-macos-arm64` |
| macOS (Intel) | `histv-cli-macos-x64` |

Place it anywhere on your PATH. On Linux/macOS, make it executable: `chmod +x histv-cli-*`

**Prerequisite:** ffmpeg and ffprobe must be installed and on your PATH. The CLI does not bundle or download ffmpeg - install it via your package manager (`apt install ffmpeg`, `brew install ffmpeg`, `choco install ffmpeg`, etc.).

## Quick start

```bash
# Encode a folder with defaults (HEVC, 5Mbps target, CQP 20/22 for below-target)
histv-cli /path/to/videos/

# Preview the plan without encoding
histv-cli --dry-run /path/to/videos/

# Custom settings
histv-cli --codec hevc --bitrate 3 --output /encoded/ /path/to/videos/

# Multiple inputs
histv-cli --bitrate 3 file1.mkv file2.mp4 /folder/of/videos/

# From a job file
histv-cli --job batch.json

# Non-interactive (for scripts and cron jobs)
histv-cli --overwrite skip --fallback yes /path/to/videos/ 2>&1 | tee encode.log
```

## Options

### Input

| Flag | Description |
|------|-------------|
| `[INPUTS]...` | Files and/or folders to encode |
| `-j, --job <FILE>` | Load settings and file list from a JSON job file |
| `--export-job <FILE>` | Write current flags + inputs to a job file, then exit |

### Video

| Flag | Description | Default |
|------|-------------|---------|
| `-c, --codec <CODEC>` | Video codec family (`hevc`, `h264`) | `hevc` |
| `-e, --encoder <NAME>` | Force a specific encoder (e.g. `hevc_nvenc`, `libx265`). Omit to auto-detect. | auto |
| `-b, --bitrate <MBPS>` | Target bitrate in Mbps | `5` |
| `--rc <MODE>` | Rate-control for below-target transcodes (`qp`, `crf`) | `qp` |
| `--qp-i <N>` | QP I-frame value | `20` |
| `--qp-p <N>` | QP P-frame value | `22` |
| `--crf <N>` | CRF value (software encoders only) | `20` |
| `--hdr` | Preserve 10-bit HDR | auto-detected |
| `--no-hdr` | Force SDR output even for HDR sources | |

### Audio

| Flag | Description | Default |
|------|-------------|---------|
| `-a, --audio <CODEC>` | Audio codec (`ac3`, `eac3`, `aac`, `copy`) | `ac3` |
| `--audio-cap <KBPS>` | Audio bitrate cap | `640` |

### Output

| Flag | Description | Default |
|------|-------------|---------|
| `-o, --output <DIR>` | Output directory | `./output` |
| `--next-to-input` | Create `output/` subfolder next to each input file | |
| `--container <FMT>` | Output container (`mkv`, `mp4`) | `mkv` |
| `--overwrite <POLICY>` | When output exists: `ask`, `yes`, `skip` | `ask` |
| `--delete-source` | Delete source files after successful encode | |

### Behaviour

| Flag | Description | Default |
|------|-------------|---------|
| `--dry-run` | Probe files and print the encoding plan, then exit | |
| `--fallback <POLICY>` | HW encoder failure: `ask`, `yes`, `no` | `ask` |
| `--remote <POLICY>` | Remote share handling: `auto`, `always`, `never` | `auto` |
| `--local-tmp <DIR>` | Local staging directory for remote files | system temp |
| `--disk-limit <PCT>` | Pause when output partition exceeds this % (50-99, or `off`). Implies `--delete-source`. | `off` |
| `--disk-resume <PCT>` | Resume when usage drops below this % | free space at start |
| `--post-command <CMD>` | Shell command to run after batch completes | |
| `--save-log` | Save batch log to the output directory | |
| `--log-level <LEVEL>` | Verbosity: `quiet`, `normal`, `verbose` | `normal` |

## Encoding decisions

For each file, the encoder decides what to do based on the source bitrate relative to the target:

| Condition | Action |
|-----------|--------|
| Same codec, at or below target | **Copy** (stream-copy, no re-encode) |
| Different codec, at or below target | **CQP/CRF transcode** (quality-based) |
| Above target | **VBR transcode** (bitrate-limited, peak = 1.5x target) |

After encoding, if the output is larger than the source, the encoder discards it and remuxes the source into the target container instead.

## Job files

A JSON file containing the same settings as the CLI flags, plus a `files` array. All fields are optional - omitted fields use defaults. CLI flags override job file values.

```json
{
  "files": [
    "/srv/media/movies/",
    "/srv/media/clips/holiday.mkv"
  ],
  "codec": "hevc",
  "bitrate": 5,
  "rateControl": "qp",
  "qpI": 20,
  "qpP": 22,
  "crf": 20,
  "audioCodec": "ac3",
  "audioBitrateCap": 640,
  "output": "/srv/media/encoded/",
  "container": "mkv",
  "overwrite": "ask",
  "deleteSource": false,
  "fallback": "ask",
  "remote": "auto",
  "saveLog": true
}
```

Generate a starter job file from current flags:

```bash
histv-cli --export-job my-batch.json --codec h264 --bitrate 8 /srv/media/
```

## Remote share handling

When files reside on network mounts (NFS, CIFS/SMB, sshfs, etc.), the CLI can stage them to local storage before encoding. This avoids ffmpeg reading the entire file across the network.

| `--remote` value | Remote mount | Local disk |
|------------------|-------------|------------|
| `auto` (default) | Stage locally | Encode in-place |
| `always` | Stage locally | Stage locally |
| `never` | Encode in-place | Encode in-place |

Detection is automatic: Linux reads `/proc/mounts`, Windows checks `GetDriveTypeW` and UNC paths, macOS parses `mount` output.

The staging directory defaults to `$TMPDIR/histv-staging` (or `%TEMP%\histv-staging` on Windows). Override with `--local-tmp` or the `$HISTV_TMP` environment variable.

## Signal handling

| Signal | Effect |
|--------|--------|
| Ctrl+C (once) | Cancel current file, continue to next |
| Ctrl+C (twice within 2s) | Cancel entire batch |
| SIGTERM | Cancel batch immediately |

Partial output files are cleaned up on cancellation. Staged input copies are cleaned up via a drop guard.

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | All files succeeded or were copied |
| 1 | One or more files failed |
| 2 | Batch was cancelled (Ctrl+C / SIGTERM) |
| 3 | No files to process |
| 4 | Fatal error (ffmpeg not found, output dir not writable, etc.) |

## Non-interactive usage

When stderr is not a TTY (piped or redirected), the CLI uses safe defaults:

- `--overwrite ask` defaults to **skip**
- `--fallback ask` defaults to **yes** (auto-fallback to software encoder)
- Progress output is simple line-based (no ANSI colours or progress bars)

For fully unattended operation:

```bash
histv-cli --overwrite skip --fallback yes --log-level quiet /path/to/videos/
```

## Disk-space monitoring

Use `--disk-limit` to enable disk-aware mode. The CLI pauses encoding when the output partition exceeds the specified usage percentage, and resumes when space recovers.

```bash
# Pause at 90% disk usage, auto-delete sources to free space
histv-cli --disk-limit 90 /path/to/videos/
```

`--disk-limit` implies `--delete-source` because space can only recover if completed source files are removed.

The `--dry-run` output includes a disk-space estimate showing projected usage with and without `--delete-source`.

## Building from source

```bash
# CLI only (no GUI dependencies)
cd src-tauri
cargo build --release --bin histv-cli --no-default-features --features cli

# Both GUI and CLI
cargo tauri build
cd src-tauri
cargo build --release --bin histv-cli --no-default-features --features cli
```

The CLI binary has no dependency on Tauri, GTK, WebKit, or any GUI framework. It links only against system libraries (libc on Unix, kernel32 on Windows) plus the Rust standard library.
