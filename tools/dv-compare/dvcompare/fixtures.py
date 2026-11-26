"""Synthetic fixtures for the self-test: a tiny generated clip, test RPUs from dvc-rpu, and the two joined into
Dolby Vision MP4 files. Nothing here reads a real video."""

import re
import shutil
import struct
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


def dv_mp4(tools, clip_path, rpu_list, dest, dvp="8.1", order="display", inband=False):
    """Put one RPU after the last slice of every access unit of `clip_path` and write a Dolby Vision MP4.
    order="display": RPU k goes to the k-th frame in display order; "decode": to the k-th packet in file order.
    inband=True keeps the parameter sets in the samples (MP4Box `xps_inband`)."""
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
    p = media.run(
        ["MP4Box", "-add", f"{annexb}:dvp={dvp}:fps=24" + (":xps_inband" if inband else ""), "-new", dest], check=False
    )
    annexb.unlink()
    if p.returncode != 0:
        raise media.ToolError("MP4Box failed: " + p.stderr.decode("utf-8", "replace").strip()[-200:])
    return dest


def _boxes(data, start, end):
    """(type, payload start, end) of each box in data[start:end]."""
    at = start
    while at + 8 <= end:
        size, kind = struct.unpack(">I4s", data[at : at + 8])
        head = 8
        if size == 1:
            size, head = struct.unpack(">Q", data[at + 8 : at + 16])[0], 16
        if size < head:
            break
        yield kind, at + head, at + size
        at += size


def _find(data, path, start=0, end=None):
    """Payload ranges of every box at `path` (b"moov", b"trak", ...)."""
    end = len(data) if end is None else end
    for kind, a, b in _boxes(data, start, end):
        if kind == path[0]:
            if len(path) == 1:
                yield a, b
            else:
                yield from _find(data, path[1:], a, b)


def _sample_nals(sample):
    """(type, bytes including the 4-byte length) of each NAL unit in an MP4 sample."""
    at = 0
    while at + 5 <= len(sample):
        end = at + 4 + struct.unpack(">I", sample[at : at + 4])[0]
        yield (sample[at + 4] >> 1) & 0x3F, sample[at:end]
        at = end


def trailing_parameter_sets(src, dest):
    """Copy the MP4 `src` (one video track, parameter sets in the first sample, 4-byte NAL lengths) so that the
    sample before every later keyframe ends with the VPS, SPS and PPS, after its RPU. Some Dolby Vision files are
    stored this way. Returns the number of samples that now end in parameter sets."""
    data = Path(src).read_bytes()
    stbl = list(_find(data, [b"moov", b"trak", b"mdia", b"minf", b"stbl"]))
    mdat = list(_find(data, [b"mdat"]))
    if len(stbl) != 1 or len(mdat) != 1:
        raise media.ToolError(f"fixture: expected one track and one mdat, found {len(stbl)} and {len(mdat)}")
    tables = {kind: a for kind, a, _ in _boxes(data, *stbl[0])}
    if not {b"stsz", b"stsc", b"stco"} <= set(tables):
        raise media.ToolError("fixture: the track needs stsz, stsc and stco tables")

    def u32(at):
        return struct.unpack(">I", data[at : at + 4])[0]

    stsz, stsc, stco = tables[b"stsz"], tables[b"stsc"], tables[b"stco"]
    count = u32(stsz + 8)
    runs = [(u32(stsc + 8 + 12 * i), u32(stsc + 12 + 12 * i)) for i in range(u32(stsc + 4))]
    chunks, samples = [], []  # index of each chunk's first sample; sample bytes in file order
    for c in range(u32(stco + 4)):
        at = u32(stco + 8 + 4 * c)
        chunks.append(len(samples))
        for _ in range([n for first, n in runs if first <= c + 1][-1]):
            if len(samples) < count:
                size = u32(stsz + 12 + 4 * len(samples))
                samples.append(data[at : at + size])
                at += size
    if len(samples) != count or u32(stsz + 4) != 0 or u32(stco + 8) != mdat[0][0]:
        raise media.ToolError("fixture: unexpected sample layout")
    sets = b"".join(n for t, n in _sample_nals(samples[0]) if t in (32, 33, 34))
    moved = 0
    for k in range(1, count):
        if sets and any(16 <= t <= 21 for t, _ in _sample_nals(samples[k])):
            samples[k - 1] += sets
            moved += 1
    if not moved:
        raise media.ToolError("fixture: no parameter sets in the first sample, or no later keyframe")
    out = bytearray(data[: mdat[0][0]] + b"".join(samples) + data[mdat[0][1] :])
    grown = len(out) - len(data)
    out[mdat[0][0] - 8 : mdat[0][0] - 4] = struct.pack(">I", mdat[0][1] - mdat[0][0] + 8 + grown)
    shift = grown if stsz > mdat[0][1] else 0  # the tables moved if moov follows mdat
    at = mdat[0][0]
    for k, sample in enumerate(samples):
        out[stsz + shift + 12 + 4 * k : stsz + shift + 16 + 4 * k] = struct.pack(">I", len(sample))
        if k in chunks:
            c = chunks.index(k)
            out[stco + shift + 8 + 4 * c : stco + shift + 12 + 4 * c] = struct.pack(">I", at)
        at += len(sample)
    Path(dest).write_bytes(bytes(out))
    return moved
