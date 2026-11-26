"""dv-compare: did the Dolby Vision data and the picture survive a transcode?

Exit codes: 0 every check passed, 1 at least one check found a difference, 2 nothing Dolby Vision was
compared, 3 the tool itself failed (missing binary, unreadable file, broken assumption).
"""

import argparse
import json
import os
import shutil
import sys
import tempfile

from . import checks as C
from . import media

EXIT_OK, EXIT_DIFF, EXIT_NOTHING, EXIT_ERROR = 0, 1, 2, 3


def compare(tools, input_path, output_path, opts, skip=(), scratch=None):
    """Run the checks. Returns (results, exit_code)."""
    for p in (input_path, output_path):
        if not os.path.isfile(p):
            raise media.ToolError(f"no such file: {p}")
    want = set(C.ALL_CHECKS) - set(skip)
    own = scratch is None
    scratch = scratch or tempfile.mkdtemp(prefix="dv-compare-")
    try:
        a = C.gather(tools, input_path, scratch, "in", want)
        b = C.gather(tools, output_path, scratch, "out", want)
        if "quality" in want and opts.mode == "reencode":
            pa, pb = a["probe"]["stream"], b["probe"]["stream"]
            size = None
            if (pa["width"], pa["height"]) != (pb["width"], pb["height"]):
                size = (pa["width"], pa["height"])
            b["quality"] = media.quality(tools, input_path, output_path, opts.start, opts.frames, size)
    finally:
        if own:
            shutil.rmtree(scratch, ignore_errors=True)
    results = []
    for name in C.ALL_CHECKS:
        if name in skip:
            results.append(C.Result(name, C.SKIP, "skipped by --skip"))
        else:
            results.append(C.CHECKS[name](a, b, opts))
    notes = [n for n in (a["rank_note"], b["rank_note"]) if n]
    return results, notes


def verdict(results):
    ran = [r for r in results if r.status != C.SKIP]
    dv_ran = [r for r in results if r.name in C.DV_CHECKS and r.status != C.SKIP]
    if any(r.status == C.DIFFERENT for r in results):
        return EXIT_DIFF
    if not ran or not dv_ran:
        return EXIT_NOTHING
    return EXIT_OK


def render(results, notes, code):
    lines = []
    for r in results:
        lines.append(f"{r.status:<9} {r.name:<12} {r.summary}")
        lines += [f"            {d}" for d in r.details]
    lines += [f"note: {n}" for n in notes]
    lines.append(
        {
            EXIT_OK: "result: all checks as expected",
            EXIT_DIFF: "result: differences found ("
            + ", ".join(r.name for r in results if r.status == C.DIFFERENT)
            + ")",
            EXIT_NOTHING: "result: nothing Dolby Vision to compare",
        }[code]
    )
    return "\n".join(lines)


def parser():
    p = argparse.ArgumentParser(prog="dv-compare", description=__doc__.split("\n\n")[0])
    sub = p.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("compare", help="compare an input video with a transcoded output")
    c.add_argument("input")
    c.add_argument("output")
    c.add_argument(
        "--mode",
        choices=("reencode", "copy"),
        default="reencode",
        help="copy: also require identical video packets; reencode (default): measure PSNR and SSIM",
    )
    c.add_argument("--start", type=int, default=0, help="first frame of the PSNR/SSIM window (default 0)")
    c.add_argument("--frames", type=int, default=48, help="length of the PSNR/SSIM window (default 48)")
    c.add_argument("--min-psnr", type=float, default=35.0, help="PSNR floor in dB (default 35)")
    c.add_argument("--min-ssim", type=float, default=0.95, help="SSIM floor (default 0.95)")
    c.add_argument("--ts-tolerance-ms", type=float, default=1.0, help="timestamp tolerance (default 1 ms)")
    c.add_argument(
        "--ignore",
        action="append",
        default=[],
        metavar="container.FIELD",
        help="leave a container field out of the verdict, e.g. container.codec_tag_string",
    )
    c.add_argument("--skip", default="", metavar="CHECK,CHECK", help="skip checks: " + ", ".join(C.ALL_CHECKS))
    c.add_argument("--json", metavar="FILE", help="also write the results as JSON")
    for t in ("ffmpeg", "ffprobe", "dvc-rpu"):
        c.add_argument(f"--{t}", help=f"path to {t} (env: {t.upper().replace('-', '_')})")
    s = sub.add_parser("selftest", help="negative controls on synthetic fixtures")
    s.add_argument("--keep", action="store_true", help="keep the fixtures (path printed)")
    for t in ("ffmpeg", "ffprobe", "dvc-rpu"):
        s.add_argument(f"--{t}")
    return p


def main(argv=None):
    args = parser().parse_args(argv)
    try:
        tools = media.Tools.find(args.ffmpeg, args.ffprobe, args.dvc_rpu)
        if args.cmd == "selftest":
            from . import selftest

            return selftest.run(tools, args.keep)
        skip = tuple(x for x in args.skip.split(",") if x)
        for x in skip:
            if x not in C.ALL_CHECKS:
                raise media.ToolError(f"unknown check {x!r}")
        opts = C.Opts(
            mode=args.mode,
            ts_tol_ms=args.ts_tolerance_ms,
            start=args.start,
            frames=args.frames,
            min_psnr=args.min_psnr,
            min_ssim=args.min_ssim,
            ignore=tuple(args.ignore),
        )
        results, notes = compare(tools, args.input, args.output, opts, skip)
        code = verdict(results)
        print(render(results, notes, code))
        if args.json:
            with open(args.json, "w") as f:
                json.dump(
                    {"exit": code, "notes": notes, "results": [r.__dict__ for r in results]}, f, indent=1, default=str
                )
        return code
    except media.ToolError as e:
        print(f"dv-compare: {e}", file=sys.stderr)
        return EXIT_ERROR
