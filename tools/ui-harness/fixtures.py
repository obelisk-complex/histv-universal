"""Fixtures for the mocked Tauri bridge. Same data for every ref, so renders are comparable."""
import json
from pathlib import Path

# Mirrors AppConfig (src-tauri/src/config.rs) defaults, camelCase as on the wire.
CONFIG_DEFAULTS = {
    "theme": "Default Dark",
    "outputFolder": "output",
    "outputMode": "folder",
    "overwrite": False,
    "deleteSource": False,
    "saveLog": False,
    "showToast": False,
    "postAction": "None",
    "postCountdown": 0,
    "customCommand": "",
    "videoCodec": "HEVC",
    "targetBitrate": 4,
    "qpI": 20,
    "qpP": 22,
    "crf": 20,
    "rateControlMode": "QP",
    "hdr": False,
    "audioCodec": "AC3",
    "audioBitrateCap": 640,
    "autoClearCompleted": False,
    "logDrawerOpen": False,
    "peakMultiplier": 1.5,
    "threads": 0,
    "lowPriority": False,
    "precisionMode": False,
    "compatibilityMode": False,
    "preserveAv1": False,
    "forceLocal": False,
}

ENCODERS = [
    {"name": "hevc_nvenc", "codecFamily": "hevc", "isHardware": True},
    {"name": "h264_nvenc", "codecFamily": "h264", "isHardware": True},
    {"name": "av1_nvenc", "codecFamily": "av1", "isHardware": True},
    {"name": "libx265", "codecFamily": "hevc", "isHardware": False},
    {"name": "libx264", "codecFamily": "h264", "isHardware": False},
]


def _row(file_name, status, codec, w, h, mbps, size_mb, **extra):
    base = {
        "fullPath": f"/home/user/Videos/{file_name}",
        "fileName": file_name,
        "baseName": file_name.rsplit(".", 1)[0],
        "status": status,
        "sourceBytes": int(size_mb * 1_000_000),
        "videoCodec": codec,
        "videoWidth": w,
        "videoHeight": h,
        "videoBitrateBps": mbps * 1_000_000,
        "videoBitrateMbps": mbps,
        "isHdr": False,
        "colorTransfer": "bt709",
        "audioStreams": [{"index": 1, "codec": "eac3", "bitrateKbps": 640}],
        "durationSecs": 3600.0,
        "doviProfile": None,
        "doviBlCompatId": None,
        "hasHdr10plus": False,
        "hdr10plusProfile": "",
        "videoRotation": 0,
        "videoFps": 23.976,
        "subtitleStreamCount": 2,
    }
    base.update(extra)
    return base


# One row per status. The harness fires the matching events so that row 1 also shows progress.
QUEUE = [
    _row("Holiday.2024.2160p.HDR.mkv", "Pending", "hevc", 3840, 2160, 18.4, 11200,
         isHdr=True, colorTransfer="smpte2084"),
    _row("Documentary.S01E03.1080p.mkv", "Encoding", "h264", 1920, 1080, 9.1, 4100),
    _row("Concert.Film.DolbyVision.mkv", "Done", "hevc", 3840, 2160, 24.0, 15800,
         isHdr=True, colorTransfer="smpte2084", doviProfile=8, doviBlCompatId=1),
    _row("Broken.Sample.mkv", "Failed", "mpeg4", 720, 480, 1.2, 310),
    _row("Already.Small.Clip.mp4", "Skipped", "av1", 1280, 720, 1.8, 220),
    _row("Nature.HLG.Test.mkv", "Cancelled", "hevc", 3840, 2160, 22.5, 9400,
         isHdr=True, colorTransfer="arib-std-b67"),
    _row("A.Very.Long.File.Name.That.Should.Truncate.In.The.Queue.Table.Because.It.Is.Long.mkv",
         "Pending", "h264", 1920, 1080, 6.3, 2600),
]

LOG_LINES = [
    "[detect] hevc_nvenc: available",
    "CMD: ffmpeg -i Documentary.S01E03.1080p.mkv -c:v hevc_nvenc -qp 20 out.mkv",
    "Encoding file 2 of 5",
    "WARNING: no HDR10+ tool found; metadata will be dropped",
    "ERROR: Broken.Sample.mkv: could not read stream info",
    "Finished Concert.Film.DolbyVision.mkv",
]

PREFLIGHT_WARNINGS = [
    {"fileName": "Concert.Film.DolbyVision.mkv", "sourceType": "Dolby Vision 8.1",
     "bestPossible": "Dolby Vision kept", "actualOutcome": "HDR10 only", "missingTool": "MP4Box"},
    {"fileName": "Holiday.2024.2160p.HDR.mkv", "sourceType": "HDR10+",
     "bestPossible": "HDR10+ kept", "actualOutcome": "HDR10 only", "missingTool": None},
]

DIALOG_PATHS = [q["fullPath"] for q in QUEUE]


def themes_from_tree(tree_root: Path):
    """The themes a backend built from this ref would serve (src-tauri/themes/*.json)."""
    out = []
    folder = tree_root / "src-tauri" / "themes"
    if folder.is_dir():
        for p in sorted(folder.glob("*.json")):
            t = json.loads(p.read_text())
            out.append({"name": t["name"], "colors": t["colors"]})
    return out


def build(themes, theme, ui_style=None, delays=None, drop=None, fake_clock=True):
    config = dict(CONFIG_DEFAULTS, theme=theme)
    if ui_style is not None:
        config["uiStyle"] = ui_style
    return {
        "config": config,
        "themes": themes,
        "queue": QUEUE,
        "encoders": ENCODERS,
        "audioEncoders": ["ac3", "eac3", "aac"],
        "dialogPaths": DIALOG_PATHS,
        "preflightWarnings": [],
        "delays": delays or {},
        "drop": drop or [],
        "fakeClock": fake_clock,
    }
