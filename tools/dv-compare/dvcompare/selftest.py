"""Negative controls. Each damaged copy of a synthetic Dolby Vision clip must make dv-compare exit 1 and name the
check that caught it; the undamaged copies must exit 0. A control that does not fail where it should fails the
self-test."""

import os
import shutil
import tempfile
from pathlib import Path

from . import checks as C
from . import cli, fixtures, media

FRAMES = 48
L1_FRAME = 7  # display frame whose L1 block the "one value changed" control alters


def _ffmpeg(tools, src, dest, *args):
    media.run(
        [
            tools.ffmpeg,
            "-nostdin",
            "-v",
            "error",
            "-y",
            "-i",
            src,
            "-map",
            "0",
            "-c",
            "copy",
            "-strict",
            "unofficial",
            *args,
            dest,
        ]
    )
    return dest


def _remux(src, dest):
    media.run(["MP4Box", "-add", f"{src}#video", "-new", dest])
    return dest


def _flip_mid_mdat(src, dest):
    data = bytearray(Path(src).read_bytes())
    at = int(len(data) * 0.6)
    for i in range(at, at + 8):
        data[i] ^= 0xFF
    Path(dest).write_bytes(bytes(data))
    return dest


def _truncate(src, dest):
    data = Path(src).read_bytes()
    Path(dest).write_bytes(data[: int(len(data) * 0.7)])
    return dest


def run(tools, keep=False):
    work = tempfile.mkdtemp(prefix="dv-compare-selftest-")

    def w(name):
        return os.path.join(work, name)

    try:
        base = fixtures.clip(tools, w("base.mp4"))
        other = fixtures.clip(tools, w("other.mp4"), bframes=2, crf=22)
        poor = fixtures.clip(tools, w("poor.mp4"), crf=48)
        good = fixtures.rpus(tools, w("good.rpu"), FRAMES)
        l1 = fixtures.rpus(tools, w("l1.rpu"), FRAMES, variant=f"l1@{L1_FRAME}")
        src = fixtures.dv_mp4(tools, base, good, w("src.mp4"))

        # (name, output builder, mode, expected exit, check that must report DIFFERENT)
        controls = [
            ("copy, remuxed with MP4Box", lambda: _remux(src, w("c1.mp4")), "copy", 0, None),
            (
                "re-encode, other GOP, RPUs re-attached by display order",
                lambda: fixtures.dv_mp4(tools, other, good, w("c2.mp4")),
                "reencode",
                0,
                None,
            ),
            (
                "RPUs removed (filter_units=remove_types=62)",
                lambda: _ffmpeg(tools, w("c1.mp4"), w("n1.mp4"), "-bsf:v", "filter_units=remove_types=62"),
                "copy",
                1,
                "rpu_present",
            ),
            (
                "sample entry tag changed to hev1",
                lambda: _ffmpeg(tools, src, w("n2.mp4"), "-tag:v", "hev1"),
                "copy",
                1,
                "container",
            ),
            (
                "one L1 value changed on one frame",
                lambda: fixtures.dv_mp4(tools, base, l1, w("n3.mp4")),
                "copy",
                1,
                "ext_blocks",
            ),
            (
                "RPUs attached in decode order instead of display order",
                lambda: fixtures.dv_mp4(tools, base, good, w("n4.mp4"), order="decode"),
                "copy",
                1,
                "ext_blocks",
            ),
            (
                "Dolby Vision configuration box says 8.4",
                lambda: fixtures.dv_mp4(tools, base, good, w("n5.mp4"), dvp="8.4"),
                "copy",
                1,
                "dv_config",
            ),
            (
                "timestamps doubled",
                lambda: _ffmpeg(tools, w("c1.mp4"), w("n6.mp4"), "-bsf:v", "setts=ts=TS*2"),
                "copy",
                1,
                "timestamps",
            ),
            ("bytes flipped inside the video data", lambda: _flip_mid_mdat(src, w("n7.mp4")), "copy", 1, "packets_md5"),
            (
                "last 18 frames cut off",
                lambda: _ffmpeg(tools, w("c1.mp4"), w("n8.mp4"), "-frames:v", str(FRAMES - 18)),
                "copy",
                1,
                "container",
            ),
            ("file truncated to 70 percent", lambda: _truncate(src, w("n9.mp4")), "copy", 1, "container"),
            (
                "re-encode far below the quality floor",
                lambda: fixtures.dv_mp4(tools, poor, good, w("n10.mp4")),
                "reencode",
                1,
                "quality",
            ),
            ("no Dolby Vision in either file (exit 2)", lambda: _ffmpeg(tools, base, w("n11.mp4")), "copy", 2, None),
        ]
        failures, rows = [], []
        for name, build, mode, want_exit, want_check in controls:
            try:
                out = build()
                src_file = base if want_exit == 2 else src
                results, _ = cli.compare(tools, src_file, out, C.Opts(mode=mode), scratch=work)
                got = cli.verdict(results)
                differing = {r.name for r in results if r.status == C.DIFFERENT}
                ok = got == want_exit and (want_check is None or want_check in differing)
                if want_exit == 0:
                    ok = ok and not differing
                detail = f"exit {got}, differing: {', '.join(sorted(differing)) or 'none'}"
            except media.ToolError as e:
                ok, detail = False, f"tool error: {e}"
            rows.append((ok, name, want_exit, want_check, detail))
            if not ok:
                failures.append(name)

        # exit 3: a missing input
        try:
            cli.compare(tools, w("absent.mp4"), src, C.Opts())
            ok3, d3 = False, "no error raised"
        except media.ToolError as e:
            ok3, d3 = True, str(e)
        rows.append((ok3, "input file missing (exit 3)", 3, None, d3))
        if not ok3:
            failures.append("input file missing")

        for ok, name, want_exit, want_check, detail in rows:
            expect = f"exit {want_exit}" + (f" naming {want_check}" if want_check else "")
            print(f"{'PASS' if ok else 'FAIL'}  {name}: expected {expect}; got {detail}")
        print(f"selftest: {len(rows) - len(failures)} of {len(rows)} controls behaved")
        return 0 if not failures else 1
    finally:
        if keep:
            print(f"fixtures kept in {work}")
        else:
            shutil.rmtree(work, ignore_errors=True)
