# Automatically Shrink Your Media Library with HISTV and Sonarr/Radarr

This guide explains how to set up [Honey, I Shrunk The Vids](https://github.com/obelisk-complex/histv-universal) (HISTV) so that every time Sonarr or Radarr downloads a new TV episode or movie, HISTV automatically re-encodes it to save disk space - without you having to lift a finger.

## What this does (and doesn't do)

When Sonarr downloads a new episode of your favourite show (or Radarr grabs a movie), the file lands in your media library. Normally that's the end of the story. With this setup, a small script runs automatically after each download and asks HISTV to check whether the file is larger than it needs to be:

- **If the file is already small enough** - nothing happens. HISTV copies it as-is, no quality loss.
- **If the file is too big** - HISTV re-encodes it using your GPU (or CPU) to bring the bitrate down to your target, then replaces the original.
- **If the file is a different codec but already small** - it stays untouched. HISTV won't re-encode a small H.264 file just because you asked for HEVC; that would risk losing quality for no benefit.

This all happens in the background. You don't need to open HISTV or do anything manually.

## What you need before starting

1. **Sonarr** (for TV) and/or **Radarr** (for movies), already set up and working.
   - [Sonarr website](https://sonarr.tv/)
   - [Radarr website](https://radarr.video/)

2. **histv-cli** - the command-line version of HISTV. Download it from the [Releases page](https://github.com/obelisk-complex/histv-universal/releases). You want the file called `histv-cli` (Linux/macOS) or `histv-cli.exe` (Windows).

3. **ffmpeg and ffprobe** - the tools HISTV uses to actually encode video. Most Linux systems can install these with `sudo apt install ffmpeg`. On Windows, download a static build from [gyan.dev](https://www.gyan.dev/ffmpeg/builds/) or [BtbN's GitHub builds](https://github.com/BtbN/FFmpeg-Builds/releases).

All three tools must be accessible to whatever user account runs Sonarr/Radarr. If you can open a terminal, type `histv-cli --version` and `ffmpeg -version` and get responses from both, you're good.

## How it works

Sonarr and Radarr have a feature called **Custom Scripts** (under Settings > Connect). You can tell them to run a script every time a file is imported. The script receives information about the file (including its full path) through special variables called "environment variables". You don't need to understand how these work; just know that the script uses them to find out which file was just downloaded.

The script we'll create simply takes that file path and passes it to `histv-cli` with the right settings.

For full technical details on custom scripts, see the official documentation:

- [Sonarr Custom Scripts](https://wiki.servarr.com/sonarr/custom-scripts) (Servarr Wiki)
- [Radarr Custom Scripts](https://wiki.servarr.com/radarr/custom-scripts) (Servarr Wiki)

## Step 1: Create the script

You only need one script. It works with both Sonarr and Radarr.

### Linux and macOS

Open a text editor and paste the following. Save it as `/opt/scripts/histv-on-import.sh` (or anywhere you like, just remember the path).

```bash
#!/bin/bash
# HISTV post-import encoder for Sonarr and Radarr.
# This script runs automatically after a file is imported.

# ── Work out which file was just imported ───────────────────

# Sonarr sets "sonarr_eventtype", Radarr sets "radarr_eventtype".
# We check which one exists to figure out who called us.

if [ -n "$sonarr_eventtype" ]; then
    EVENT="$sonarr_eventtype"
    FILE="$sonarr_episodefile_path"
elif [ -n "$radarr_eventtype" ]; then
    EVENT="$radarr_eventtype"
    FILE="$radarr_moviefile_path"
else
    # Called by something else (e.g. the Test button) - just exit quietly
    exit 0
fi

# Only run when a file is actually imported (not on grab, rename, etc.)
if [ "$EVENT" != "Download" ]; then
    exit 0
fi

# Make sure the file actually exists
if [ -z "$FILE" ] || [ ! -f "$FILE" ]; then
    echo "File not found: $FILE" >&2
    exit 1
fi

# ── Run HISTV ───────────────────────────────────────────────

histv-cli \
    --output-mode replace \
    --codec hevc \
    --bitrate 4 \
    --container mkv \
    --overwrite yes \
    --fallback yes \
    --log-level quiet \
    "$FILE"
```

After saving, make the script executable by running this in a terminal:

```bash
chmod +x /opt/scripts/histv-on-import.sh
```

### Windows

You need two small files. The first is a launcher that Sonarr/Radarr can call directly; the second is the real script.

Save this as `C:\Scripts\histv-on-import.bat`:

```batch
@echo off
REM HISTV post-import encoder for Sonarr and Radarr (Windows).
REM This .bat file calls a PowerShell script that does the real work.
powershell.exe -ExecutionPolicy Bypass -File "C:\Scripts\histv-on-import.ps1"
exit /b %ERRORLEVEL%
```

Save this as `C:\Scripts\histv-on-import.ps1`:

```powershell
# HISTV post-import encoder for Sonarr and Radarr.

# ── Work out which file was just imported ───────────────────

$EventType = $null
$FilePath  = $null

if ($env:sonarr_eventtype) {
    $EventType = $env:sonarr_eventtype
    $FilePath  = $env:sonarr_episodefile_path
}
elseif ($env:radarr_eventtype) {
    $EventType = $env:radarr_eventtype
    $FilePath  = $env:radarr_moviefile_path
}
else {
    exit 0
}

# Only run when a file is actually imported
if ($EventType -ne "Download") { exit 0 }

# Make sure the file exists
if (-not $FilePath -or -not (Test-Path -LiteralPath $FilePath)) {
    Write-Error "File not found: $FilePath"
    exit 1
}

# ── Run HISTV ───────────────────────────────────────────────

& histv-cli `
    --output-mode replace `
    --codec hevc `
    --bitrate 4 `
    --container mkv `
    --overwrite yes `
    --fallback yes `
    --log-level quiet `
    "$FilePath"

exit $LASTEXITCODE
```

**Why two files on Windows?** Sonarr and Radarr on Windows expect to run a `.bat` or `.exe` file directly. The `.bat` file is a one-line launcher that calls the PowerShell script where the real logic lives.

## Step 2: Tell Sonarr/Radarr to use the script

The steps are the same in both Sonarr and Radarr. Here's what to do in Sonarr (repeat the same process for Radarr if you use both):

1. Open Sonarr's web interface (usually `http://your-server:8989`)
2. Go to **Settings** in the left sidebar
3. Click **Connect**
4. Click the **+** button to add a new connection
5. Choose **Custom Script** from the list
6. Fill in the form:
   - **Name:** `HISTV Encode` (or whatever you like)
   - **On Import:** ticked
   - **On Upgrade:** ticked
   - **On Grab:** unticked
   - **On Rename:** unticked
   - **Path:** the full path to your script
     - Linux/macOS: `/opt/scripts/histv-on-import.sh`
     - Windows: `C:\Scripts\histv-on-import.bat`
7. Click **Test** - you should see a green tick. (The test sends a fake event that the script safely ignores.)
8. Click **Save**

That's it. The next time Sonarr imports an episode, HISTV will automatically encode it.

For Radarr, the steps are identical but at `http://your-server:7878`.

## Step 3: Choose your settings

The script above uses these defaults, which work well for most people:

| Setting | Value | What it means |
|---------|-------|---------------|
| `--codec hevc` | HEVC/H.265 | Modern codec, produces smaller files. If your TV or player can't handle HEVC, change to `h264`. |
| `--bitrate 4` | 4 Mbps | Files above 4 Mbps get shrunk. Good for 1080p content. For 4K, try `8` or `10`. For aggressive space saving, try `2` or `3`. |
| `--container mkv` | MKV format | MKV supports virtually everything. Use `mp4` if your player prefers it. |
| `--fallback yes` | Auto-fallback | If GPU encoding fails, automatically retry with CPU. No manual intervention needed. |

### Example: different settings for different needs

**"I mostly watch on a smart TV and want small files"**

Change the `histv-cli` line in the script to:
```
histv-cli --output-mode replace --codec hevc --bitrate 3 --container mkv --overwrite yes --fallback yes --log-level quiet "$FILE"
```
This targets 3 Mbps, which gives good quality on most TVs while keeping files compact.

**"I stream to lots of different devices and want maximum compatibility"**

```
histv-cli --output-mode replace --codec h264 --bitrate 6 --container mp4 --overwrite yes --fallback yes --log-level quiet "$FILE"
```
H.264 and MP4 play on virtually every device made in the last 15 years.

**"I have 4K content and want to keep it looking great"**

```
histv-cli --output-mode replace --codec hevc --bitrate 10 --container mkv --overwrite yes --fallback yes --log-level quiet "$FILE"
```
A higher bitrate target preserves the extra detail in 4K footage.

**"I want the best possible quality and don't mind slower encodes"**

```
histv-cli --output-mode replace --codec hevc --bitrate 5 --precision --container mkv --overwrite yes --fallback yes --log-level quiet "$FILE"
```
The `--precision` flag makes HISTV test clips from the file first and use advanced encoding strategies. It's slower but produces the best results.

To change a setting, just edit the script file. You don't need to redo the Sonarr/Radarr setup.

## Testing without encoding anything

Before committing to real encodes, you can add `--dry-run` to the histv-cli command in your script. This makes HISTV report what it *would* do with each file without actually touching anything.

For example, on Linux:
```bash
histv-cli \
    --output-mode replace \
    --codec hevc \
    --bitrate 4 \
    --container mkv \
    --overwrite yes \
    --fallback yes \
    --dry-run \
    "$FILE"
```

Check the Sonarr/Radarr logs (System > Logs) to see the output. Script output from `stdout` appears as Debug-level entries, and `stderr` appears as Info-level. Once you're happy with what HISTV reports, remove `--dry-run` from the script to start encoding for real.

## Things to be aware of

### Encoding takes time

While HISTV is encoding a file, Sonarr/Radarr will wait for it to finish before processing the next import. For a single episode this is usually fine - a few minutes with GPU encoding, perhaps 10-15 minutes on CPU for a typical episode. If an entire season arrives at once, each episode will be encoded one after the other, and the import queue will be paused during that time. Nothing will be lost, it just takes a while.

### Container format mismatches

If Sonarr downloads a `.mp4` file but your script encodes to `.mkv` (or the other way round), the filename extension changes. Sonarr might then report the file as "missing" because it's looking for the old name.

The easiest fix: match your `--container` setting to whatever format most of your library uses. If most of your files are `.mkv`, use `--container mkv`. If they're mostly `.mp4`, use `--container mp4`.

### Disk space

HISTV encodes to a temporary file alongside the original, then swaps them. This means you briefly need enough space for both copies to exist at the same time. For a typical 2 GB episode, you'd need about 2 GB of temporary extra space.

If your drive is nearly full, add `--disk-limit 90` to the command. This tells HISTV to pause encoding if the drive exceeds 90% usage, protecting you from running out of space.

### HDR content

By default, HISTV preserves HDR - your HDR films and shows will stay HDR. If you watch on a standard (SDR) screen and want HDR content converted to SDR with proper colour mapping so it looks right, add `--no-hdr` to the command.

### Docker setups

If Sonarr/Radarr run in Docker containers, the `histv-cli` and `ffmpeg` binaries need to be available inside the container. The simplest approach is to bind-mount the binaries into the container. This is a more advanced setup; the [LinuxServer.io Sonarr](https://hub.docker.com/r/linuxserver/sonarr) and [LinuxServer.io Radarr](https://hub.docker.com/r/linuxserver/radarr) documentation explains how volumes and bind mounts work.

## Adding a log file (optional)

If you want to keep a record of what HISTV does with each file, you can redirect the output to a log file.

On Linux/macOS, change the last part of the script to:
```bash
histv-cli \
    --output-mode replace \
    --codec hevc \
    --bitrate 4 \
    --container mkv \
    --overwrite yes \
    --fallback yes \
    --save-log \
    --log-level normal \
    "$FILE" \
    >> /var/log/histv-import.log 2>&1
```

On Windows, change the PowerShell histv-cli call to:
```powershell
& histv-cli `
    --output-mode replace `
    --codec hevc `
    --bitrate 4 `
    --container mkv `
    --overwrite yes `
    --fallback yes `
    --save-log `
    --log-level normal `
    "$FilePath" *>> "C:\Scripts\histv-import.log"
```

Check these log files to see what happened with each import.

## Further reading

- [HISTV CLI documentation](https://github.com/obelisk-complex/histv-universal/blob/main/CLI-README.md) - full list of flags and features
- [Sonarr Custom Scripts](https://wiki.servarr.com/sonarr/custom-scripts) - environment variables reference
- [Radarr Custom Scripts](https://wiki.servarr.com/radarr/custom-scripts) - environment variables reference
- [Servarr Wiki](https://wiki.servarr.com/) - the main documentation hub for Sonarr, Radarr, and related tools
- [LinuxServer.io Sonarr container](https://hub.docker.com/r/linuxserver/sonarr) - if you run Sonarr in Docker
- [LinuxServer.io Radarr container](https://hub.docker.com/r/linuxserver/radarr) - if you run Radarr in Docker