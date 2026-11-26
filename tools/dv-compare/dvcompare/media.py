"""ffmpeg / ffprobe / dvc-rpu wrappers. Everything that starts a process lives here."""

import json
import os
import re
import shutil
import subprocess
from dataclasses import dataclass
from pathlib import Path


class ToolError(Exception):
    """The tool could not do its job (missing binary, unreadable file, broken assumption). Exit 3."""


@dataclass
class Tools:
    ffmpeg: str
    ffprobe: str
    rpu: str

    @staticmethod
    def find(ffmpeg=None, ffprobe=None, rpu=None):
        ff = ffmpeg or os.environ.get("FFMPEG") or "ffmpeg"
        if ffprobe or os.environ.get("FFPROBE"):
            fp = ffprobe or os.environ["FFPROBE"]
        elif os.sep in ff:
            fp = str(Path(ff).with_name("ffprobe"))
        else:
            fp = "ffprobe"
        rp = rpu or os.environ.get("DVC_RPU") or "dvc-rpu"
        out = []
        for name, path in (("ffmpeg", ff), ("ffprobe", fp), ("dvc-rpu", rp)):
            found = shutil.which(path)
            if not found:
                raise ToolError(f"{name} not found ({path}); run.sh builds dvc-rpu, FFMPEG/FFPROBE name the others")
            out.append(found)
        return Tools(*out)


def run(cmd, check=True):
    try:
        p = subprocess.run([str(c) for c in cmd], capture_output=True, check=False)
    except OSError as e:
        raise ToolError(f"cannot run {cmd[0]}: {e}") from e
    if check and p.returncode != 0:
        tail = p.stderr.decode("utf-8", "replace").strip().splitlines()[-3:]
        raise ToolError(f"{Path(str(cmd[0])).name} failed ({p.returncode}): {' | '.join(tail)}")
    return p


def probe(tools, path):
    """Video stream 0 and the format as ffprobe reports them."""
    p = run(
        [tools.ffprobe, "-v", "error", "-select_streams", "v:0", "-show_streams", "-show_format", "-of", "json", path]
    )
    j = json.loads(p.stdout)
    if not j.get("streams"):
        raise ToolError(f"{path} has no video stream")
    s = j["streams"][0]
    dovi = next(
        (d for d in s.get("side_data_list", []) if d.get("side_data_type") == "DOVI configuration record"), None
    )
    return {"stream": s, "format": j.get("format", {}), "dovi": dovi}


def packets(tools, path):
    """Video packets in file (decode) order: pts, dts, pts_time, size, flags."""
    p = run(
        [
            tools.ffprobe,
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "packet=pts,dts,pts_time,size,flags",
            "-of",
            "json",
            path,
        ]
    )
    out = []
    for k in json.loads(p.stdout).get("packets", []):

        def num(key, cast, k=k):
            v = k.get(key)
            return None if v in (None, "N/A") else cast(v)

        out.append(
            {
                "pts": num("pts", int),
                "dts": num("dts", int),
                "t": num("pts_time", float),
                "size": int(k["size"]),
                "flags": k.get("flags", ""),
            }
        )
    return out


def display_ranks(pkts):
    """rank[k] = display position of packet k, from the packet pts (dts when pts is missing).
    Returns (ranks, note); note is set when the order could not come from pts."""
    key = [p["pts"] for p in pkts]
    note = None
    if any(v is None for v in key):
        key = [p["dts"] for p in pkts]
        note = "packets have no pts; display order taken from dts (assumes no reordering)"
        if any(v is None for v in key):
            raise ToolError("video packets have neither pts nor dts")
    order = sorted(range(len(key)), key=lambda i: (key[i], i))
    ranks = [0] * len(key)
    for r, i in enumerate(order):
        ranks[i] = r
    if len(set(key)) != len(key):
        note = "two packets share a timestamp; ties broken by packet order"
    return ranks, note


def video_md5(tools, path):
    """md5 of the video packets after conversion to Annex B, so the container's NAL framing does not matter."""
    p = run(
        [
            tools.ffmpeg,
            "-nostdin",
            "-v",
            "error",
            "-i",
            path,
            "-map",
            "0:v:0",
            "-c",
            "copy",
            "-bsf:v",
            "hevc_mp4toannexb",
            "-f",
            "md5",
            "-",
        ]
    )
    m = re.search(rb"MD5=([0-9a-f]{32})", p.stdout)
    if not m:
        raise ToolError(f"no md5 from ffmpeg for {path}")
    return m.group(1).decode()


def decode(tools, path):
    """Full decode of the video stream: (frames decoded, error lines)."""
    p = run(
        [
            tools.ffmpeg,
            "-nostdin",
            "-v",
            "error",
            "-nostats",
            "-progress",
            "pipe:1",
            "-i",
            path,
            "-map",
            "0:v:0",
            "-f",
            "null",
            "-",
        ],
        check=False,
    )
    frames = 0
    for ln in p.stdout.decode("utf-8", "replace").splitlines():
        if ln.startswith("frame="):
            frames = int(ln.split("=")[1])
    errors = [ln for ln in p.stderr.decode("utf-8", "replace").splitlines() if ln.strip()]
    if p.returncode != 0:
        errors.append(f"ffmpeg exit status {p.returncode}")
    return frames, errors


def extract_rpus(tools, path, dest):
    """Annex B file with one access unit delimiter per packet and the Dolby Vision RPU NAL units (type 62)
    of that packet. The delimiters (inserted with hevc_metadata when the stream has none) are what ties an
    RPU to its packet."""
    run(
        [
            tools.ffmpeg,
            "-nostdin",
            "-v",
            "error",
            "-y",
            "-i",
            path,
            "-map",
            "0:v:0",
            "-c",
            "copy",
            "-bsf:v",
            "hevc_metadata=aud=insert,filter_units=pass_types=35|62",
            "-f",
            "hevc",
            dest,
        ]
    )
    return dest


def dump_rpus(tools, nal_file):
    p = run([tools.rpu, "dump", nal_file])
    rows, summary = [], None
    for ln in p.stdout.decode().splitlines():
        j = json.loads(ln)
        if j.get("summary"):
            summary = j
        else:
            rows.append(j)
    if summary is None:
        raise ToolError("dvc-rpu dump gave no summary")
    return rows, summary


def show_rpu(tools, nal_file, index):
    p = run([tools.rpu, "show", nal_file, str(index)])
    return p.stdout.decode()


def quality(tools, ref, test, start, frames, scale_to=None):
    """PSNR and SSIM of `test` against `ref` over `frames` frames from frame `start`.
    Both files are decoded from the beginning (trim counts frames, not seconds) and `test` is converted to
    the pixel format of `ref` (scaled to `scale_to`, a (width, height) pair, when the sizes differ)."""
    pix = probe(tools, ref)["stream"]["pix_fmt"]
    win = f"trim=start_frame={start}:end_frame={start + frames},setpts=PTS-STARTPTS"
    b = win + (f",scale={scale_to[0]}:{scale_to[1]}:flags=bicubic" if scale_to else "")
    g = f"[0:v:0]{win},format={pix},split[a1][a2];[1:v:0]{b},format={pix},split[b1][b2];[a1][b1]psnr[p];[a2][b2]ssim[s]"
    p = run(
        [
            tools.ffmpeg,
            "-nostdin",
            "-v",
            "info",
            "-nostats",
            "-i",
            ref,
            "-i",
            test,
            "-filter_complex",
            g,
            "-map",
            "[p]",
            "-f",
            "null",
            "-",
            "-map",
            "[s]",
            "-f",
            "null",
            "-",
        ]
    )
    text = p.stderr.decode("utf-8", "replace")
    ps = re.search(r"PSNR y:(\S+) u:(\S+) v:(\S+) average:(\S+) min:(\S+) max:(\S+)", text)
    ss = re.search(r"SSIM Y:(\S+) \(\S+\) U:(\S+) \(\S+\) V:(\S+) \(\S+\) All:(\S+)", text)
    if not ps or not ss:
        raise ToolError("could not read PSNR/SSIM from ffmpeg: " + " | ".join(text.strip().splitlines()[-3:]))

    def num(x):
        return float("inf") if x == "inf" else float(x)

    return {
        "psnr_y": num(ps.group(1)),
        "psnr_avg": num(ps.group(4)),
        "psnr_min": num(ps.group(5)),
        "ssim_y": num(ss.group(1)),
        "ssim_all": num(ss.group(4)),
    }
