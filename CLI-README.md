# histv-cli

Headless batch video encoder - the command-line companion to [Honey, I Shrunk The Vids](https://github.com/obelisk-complex/histv-universal).

Same encoding engine as the desktop app: smart bitrate decisions, hardware-accelerated encoding, per-stream audio handling, remote file staging, disk-space monitoring, and post-encode size checks. Designed for headless and remote servers.

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
# Encode a folder with defaults (HEVC, 5Mbps, CQP 20/22)
histv-cli /path/to/videos/

# Preview the plan without encoding
histv-cli --dry-run /path/to/videos/

# Custom settings
histv-cli --codec hevc --bitrate 3 --output /encoded/ /path/to/videos/

# Multiple inputs
histv-cli --bitrate 3 file1.mkv file2.mp4 /folder/

# From a job file
histv-cli --job batch.json

# Non-interactive (scripts / cron)
histv-cli --overwrite skip --fallback yes /path/to/videos/ 2>&1 | tee encode.log
```

## Encoding decisions

| Condition | Action |
|-----------|--------|
| Same codec, at or below target | **Copy** (stream-copy, no re-encode) |
| Different codec, at or below target | **CQP/CRF transcode** (quality-based) |
| Above target | **VBR transcode** (bitrate-limited, peak = 1.5x target) |

After encoding, if the output is larger than the source, the encoder discards it and remuxes the source into the target container instead.

## Options

<details>
<summary><strong>Input</strong></summary>

| Flag | Description |
|------|-------------|
| `[INPUTS]...` | Files and/or folders to encode |
| `-j, --job <FILE>` | Load settings and file list from a JSON job file |
| `--export-job <FILE>` | Write current flags + inputs to a job file, then exit |

</details>

<details>
<summary><strong>Video</strong></summary>

| Flag | Default | Description |
|------|---------|-------------|
| `-c, --codec <CODEC>` | `hevc` | Codec family (`hevc`, `h264`) |
| `-e, --encoder <NAME>` | auto | Force a specific encoder (e.g. `hevc_nvenc`, `libx265`) |
| `-b, --bitrate <MBPS>` | `5` | Target bitrate in Mbps |
| `--rc <MODE>` | `qp` | Rate-control for below-target transcodes (`qp`, `crf`) |
| `--qp-i <N>` | `20` | QP I-frame value |
| `--qp-p <N>` | `22` | QP P-frame value |
| `--crf <N>` | `20` | CRF value (software encoders only) |
| `--hdr` | auto | Preserve 10-bit HDR |
| `--no-hdr` | | Force SDR output even for HDR sources |

</details>

<details>
<summary><strong>Audio</strong></summary>

| Flag | Default | Description |
|------|---------|-------------|
| `-a, --audio <CODEC>` | `ac3` | Audio codec (`ac3`, `eac3`, `aac`, `copy`) |
| `--audio-cap <KBPS>` | `640` | Audio bitrate cap |

</details>

<details>
<summary><strong>Output</strong></summary>

| Flag | Default | Description |
|------|---------|-------------|
| `-o, --output <DIR>` | `./output` | Output directory |
| `--output-mode <MODE>` | `folder` | `folder`, `beside` (subfolder next to input), `replace` (encode in-place) |
| `--container <FMT>` | `mkv` | Output container (`mkv`, `mp4`) |
| `--overwrite <POLICY>` | `ask` | When output exists: `ask`, `yes`, `skip` |
| `--delete-source` | | Delete source files after successful encode |

</details>

<details>
<summary><strong>Behaviour</strong></summary>

| Flag | Default | Description |
|------|---------|-------------|
| `--dry-run` | | Probe files and print the encoding plan, then exit |
| `--fallback <POLICY>` | `ask` | HW encoder failure: `ask`, `yes`, `no` |
| `--remote <POLICY>` | `auto` | Remote share handling: `auto`, `always`, `never` |
| `--local-tmp <DIR>` | system temp | Local staging directory for remote files |
| `--disk-limit <PCT>` | `off` | Pause at this disk usage % (50-99). Implies `--delete-source`. |
| `--disk-resume <PCT>` | baseline | Resume when usage drops below this % |
| `--post-command <CMD>` | | Shell command to run after batch completes |
| `--save-log` | | Save batch log to the output directory |
| `--log-level <LEVEL>` | `normal` | `quiet`, `normal`, `verbose` |

</details>

## Job files

<details>
<summary><strong>Format and example</strong></summary>

A JSON file with the same settings as CLI flags, plus a `files` array. All fields are optional - omitted fields use defaults. CLI flags override job file values.

```json
{
  "files": ["/srv/media/movies/", "/srv/media/clips/holiday.mkv"],
  "codec": "hevc",
  "bitrate": 5,
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
  "saveLog": true
}
```

Generate a starter job file from current flags:

```bash
histv-cli --export-job my-batch.json --codec h264 --bitrate 8 /srv/media/
```

</details>

## Remote share handling

<details>
<summary><strong>Details</strong></summary>

When files reside on network mounts (NFS, CIFS/SMB, sshfs, etc.), the CLI can stage them to local storage before encoding.

| `--remote` value | Remote mount | Local disk |
|------------------|-------------|------------|
| `auto` (default) | Stage locally | Encode in-place |
| `always` | Stage locally | Stage locally |
| `never` | Encode in-place | Encode in-place |

Detection is automatic: Linux reads `/proc/mounts`, Windows checks `GetDriveTypeW` and UNC paths, macOS parses `mount` output.

Staging directory defaults to `$TMPDIR/histv-staging` (or `%TEMP%\histv-staging` on Windows). Override with `--local-tmp` or `$HISTV_TMP`.

</details>

## Signal handling

| Signal | Effect |
|--------|--------|
| Ctrl+C (once) | Cancel current file, continue to next |
| Ctrl+C (twice within 2s) | Cancel entire batch |

Partial output and staged input copies are cleaned up automatically.

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | All files succeeded or were copied |
| 1 | One or more files failed |
| 2 | Batch was cancelled |
| 3 | No files to process |
| 4 | Fatal error (ffmpeg not found, output dir not writable, etc.) |

## Non-interactive usage

When stderr is not a TTY (piped or redirected):

- `--overwrite ask` defaults to **skip**
- `--fallback ask` defaults to **yes** (auto-fallback to software)
- Progress output is line-based with no ANSI colours or progress bars

```bash
histv-cli --overwrite skip --fallback yes --log-level quiet /path/to/videos/
```

## Disk-space monitoring

`--disk-limit` pauses encoding when the output partition exceeds the specified usage percentage, and resumes when space recovers. Implies `--delete-source`.

```bash
histv-cli --disk-limit 90 /path/to/videos/
```

The `--dry-run` output includes a disk-space estimate showing projected usage with and without `--delete-source`.