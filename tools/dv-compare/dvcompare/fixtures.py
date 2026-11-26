"""Synthetic fixtures for the self-test: a tiny generated clip, test RPUs from dvc-rpu, and the two joined into
Dolby Vision MP4 files. Nothing here reads a real video."""

import re
import shutil
from pathlib import Path

from . import media

START = b"\x00\x00\x00\x01"


def _nals(data):
    """NAL units of an Annex B stream without start codes or trailing zero bytes."""
    marks = [m.start() for m in re.finditer(b"\x00\x00\x01", data)]
    return [
        data[s + 3 : (marks[k + 1] if k + 1 < len(marks) else len(data))].rstrip(b"\x00") for k, s in enumerate(marks)
    ]


def clip(tools, dest, bframes=4, crf=12, seconds=2, size="480x270", fps=24):
    """A 10-bit PQ-tagged HEVC clip with B-frames; the picture is a synthetic test pattern."""
    media.run(
        [
            tools.ffmpeg,
            "-nostdin",
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            f"testsrc2=size={size}:rate={fps}:duration={seconds}",
            "-pix_fmt",
            "yuv420p10le",
            "-c:v",
            "libx265",
            "-crf",
            str(crf),
            "-x265-params",
            (
                f"bframes={bframes}:keyint={fps}:log-level=error:colorprim=bt2020:transfer=smpte2084:"
                "colormatrix=bt2020nc:repeat-headers=1:"
                "master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1):"
                "max-cll=1000,400"
            ),
            "-f",
            "mp4",
            dest,
        ]
    )
    return dest


def rpus(tools, dest, frames, profile="8.1", variant="base"):
    media.run([tools.rpu, "gen", dest, profile, str(frames), variant])
    return [n for n in Path(dest).read_bytes().split(START) if n]


def dv_mp4(tools, clip_path, rpu_list, dest, dvp="8.1", order="display"):
    """Put one RPU after the last slice of every access unit of `clip_path` and write a Dolby Vision MP4.
    order="display": RPU k goes to the k-th frame in display order; "decode": to the k-th packet in file order."""
    if not shutil.which("MP4Box"):
        raise media.ToolError("MP4Box is needed to write the Dolby Vision configuration box")
    pk = media.packets(tools, clip_path)
    ranks, _ = media.display_ranks(pk)
    annexb = Path(dest).with_suffix(".src.hevc")
    media.run(
        [
            tools.ffmpeg,
            "-nostdin",
            "-v",
            "error",
            "-y",
            "-i",
            clip_path,
            "-map",
            "0:v:0",
            "-c",
            "copy",
            "-bsf:v",
            "hevc_metadata=aud=insert",
            "-f",
            "hevc",
            str(annexb),
        ]
    )
    aus = []
    for n in _nals(annexb.read_bytes()):
        t = (n[0] >> 1) & 0x3F
        if t == 35:
            aus.append([])
        elif t != 62:
            aus[-1].append(n)
    if len(aus) != len(pk) or len(rpu_list) != len(aus):
        raise media.ToolError(f"fixture mismatch: {len(aus)} access units, {len(pk)} packets, {len(rpu_list)} RPUs")
    out = bytearray()
    for k, nals in enumerate(aus):
        for n in nals:
            out += START + n
        out += START + rpu_list[ranks[k] if order == "display" else k]
    annexb.write_bytes(out)
    p = media.run(["MP4Box", "-add", f"{annexb}:dvp={dvp}:fps=24", "-new", dest], check=False)
    annexb.unlink()
    if p.returncode != 0:
        raise media.ToolError("MP4Box failed: " + p.stderr.decode("utf-8", "replace").strip()[-200:])
    return dest
