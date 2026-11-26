"""The checks. Each takes the two files' gathered facts and returns a Result."""

import re
from dataclasses import dataclass, field

from . import media

PASS, DIFFERENT, SKIP = "PASS", "DIFFERENT", "SKIP"

DV_CHECKS = ("dv_config", "rpu_present", "rpu_frames", "ext_blocks")
ALL_CHECKS = (
    "container",
    "timestamps",
    "dv_config",
    "rpu_present",
    "rpu_frames",
    "ext_blocks",
    "packets_md5",
    "decode",
    "quality",
)

STREAM_FIELDS = (
    "codec_name",
    "profile",
    "codec_tag_string",
    "pix_fmt",
    "width",
    "height",
    "color_range",
    "color_space",
    "color_transfer",
    "color_primaries",
    "chroma_location",
    "r_frame_rate",
)


@dataclass
class Result:
    name: str
    status: str
    summary: str
    details: list = field(default_factory=list)
    data: dict = field(default_factory=dict)


@dataclass
class Opts:
    mode: str = "reencode"  # "copy" expects identical packets; "reencode" measures PSNR and SSIM
    ts_tol_ms: float = 1.0
    start: int = 0
    frames: int = 48
    min_psnr: float = 35.0
    min_ssim: float = 0.95
    ignore: tuple = ()  # field names ("container.codec_tag_string") excluded from the verdict
    max_examples: int = 3


def _side_data(stream, kind):
    for d in stream.get("side_data_list", []):
        if d.get("side_data_type") == kind:
            return {k: v for k, v in d.items() if k != "side_data_type"}
    return None


def container(a, b, o):
    diffs, same = [], 0
    ignored = []
    for k in STREAM_FIELDS + ("mastering_display", "content_light"):
        if k == "mastering_display":
            va, vb = (_side_data(x["probe"]["stream"], "Mastering display metadata") for x in (a, b))
        elif k == "content_light":
            va, vb = (_side_data(x["probe"]["stream"], "Content light level metadata") for x in (a, b))
        else:
            va, vb = (x["probe"]["stream"].get(k) for x in (a, b))
        if va == vb:
            same += 1
        elif f"container.{k}" in o.ignore:
            ignored.append(f"{k}: {va} -> {vb} (ignored)")
        else:
            diffs.append(f"{k}: {va} -> {vb}")
    na, nb = len(a["packets"]), len(b["packets"])
    if na != nb and "container.frames" not in o.ignore:
        diffs.append(f"video packets: {na} -> {nb}")
    summary = f"{same} stream fields equal, {na} -> {nb} packets"
    return Result(
        "container",
        DIFFERENT if diffs else PASS,
        summary if not diffs else f"{len(diffs)} differ ({summary})",
        diffs + ignored,
    )


def timestamps(a, b, o):
    ta = sorted(p["t"] for p in a["packets"] if p["t"] is not None)
    tb = sorted(p["t"] for p in b["packets"] if p["t"] is not None)
    if not ta or not tb:
        return Result("timestamps", DIFFERENT, "a file has no packet timestamps")
    if len(ta) != len(tb):
        return Result("timestamps", DIFFERENT, f"{len(ta)} -> {len(tb)} timestamps, not compared")
    deltas = [abs(x - y) * 1000 for x, y in zip(ta, tb)]
    worst = max(range(len(deltas)), key=deltas.__getitem__)
    bad = sum(1 for d in deltas if d > o.ts_tol_ms)
    notes = [f"display frame {worst}: {ta[worst]:.6f}s -> {tb[worst]:.6f}s"] if bad else []
    summary = (
        f"{len(ta)} frames, first {ta[0]:.6f}s -> {tb[0]:.6f}s, last {ta[-1]:.6f}s -> {tb[-1]:.6f}s, "
        f"largest shift {deltas[worst]:.3f} ms (tolerance {o.ts_tol_ms} ms)"
    )
    return Result(
        "timestamps",
        DIFFERENT if bad else PASS,
        f"{bad} frames shifted; " + summary if bad else summary,
        notes,
        {"max_ms": deltas[worst], "shifted": bad},
    )


def dv_config(a, b, o):
    da, db = a["probe"]["dovi"], b["probe"]["dovi"]
    if da is None and db is None:
        return Result("dv_config", SKIP, "neither file has a Dolby Vision configuration box")
    keys = (
        "dv_profile",
        "dv_level",
        "dv_bl_signal_compatibility_id",
        "rpu_present_flag",
        "el_present_flag",
        "bl_present_flag",
    )
    fmt = [", ".join(f"{k.replace('dv_', '')}={d.get(k)}" for k in keys) if d else "absent" for d in (da, db)]
    same = da is not None and db is not None and all(da.get(k) == db.get(k) for k in keys)
    return Result("dv_config", PASS if same else DIFFERENT, fmt[0] if same else f"{fmt[0]} -> {fmt[1]}")


def _per_display(side):
    """RPU rows per display position. Every row is attributed to the packet (access unit) it arrived in."""
    ranks = side["ranks"]
    out = [[] for _ in ranks]
    for r in side["rpus"]:
        out[ranks[r["au"]]].append(r)
    return out


def _pairing_error(name, a, b):
    errs = [x["rpu_error"] for x in (a, b) if x.get("rpu_error")]
    return Result(name, DIFFERENT, errs[0], errs[1:]) if errs else None


def rpu_present(a, b, o):
    if err := _pairing_error("rpu_present", a, b):
        return err
    if not a["rpus"] and not b["rpus"]:
        return Result("rpu_present", SKIP, "neither file has Dolby Vision RPUs")
    pa, pb = _per_display(a), _per_display(b)

    def count(p):
        return (sum(1 for x in p if len(x) == 0), sum(1 for x in p if len(x) == 1), sum(1 for x in p if len(x) > 1))

    ca, cb = count(pa), count(pb)
    notes = [
        f"input: {ca[1]} frames with one RPU, {ca[0]} with none, {ca[2]} with several",
        f"output: {cb[1]} frames with one RPU, {cb[0]} with none, {cb[2]} with several",
    ]
    bad = len(pa) != len(pb) or any(len(x) != len(y) for x, y in zip(pa, pb))
    first = next((i for i, (x, y) in enumerate(zip(pa, pb)) if len(x) != len(y)), None)
    if first is not None:
        notes.append(f"first display frame with a different RPU count: {first} ({len(pa[first])} -> {len(pb[first])})")
    if ca[2] or cb[2]:
        notes.append("more than one RPU in a packet: the one-RPU-per-frame assumption does not hold here")
    for label, c in (("input", ca), ("output", cb)):
        if c[0] and (c is cb or cb[0] != ca[0]):
            notes.append(f"{label} has {c[0]} frames without an RPU")
    summary = f"output has an RPU on {cb[1] + cb[2]} of {len(pb)} frames (input {ca[1] + ca[2]} of {len(pa)})"
    return Result("rpu_present", DIFFERENT if bad else PASS, summary, notes, {"input": ca, "output": cb})


def _numbers(text):
    return {k: int(v) for k, v in re.findall(r"(\w+): (-?\d+)", text)}


def _pair(a, b):
    pa, pb = _per_display(a), _per_display(b)
    if len(pa) != len(pb) or any(len(x) != len(y) for x, y in zip(pa, pb)):
        return None
    return [(i, x, y) for i, (la, lb) in enumerate(zip(pa, pb)) for x, y in zip(la, lb)]


def rpu_frames(a, b, o):
    if err := _pairing_error("rpu_frames", a, b):
        return err
    if not a["rpus"] and not b["rpus"]:
        return Result("rpu_frames", SKIP, "neither file has Dolby Vision RPUs")
    pairs = _pair(a, b)
    if pairs is None:
        return Result("rpu_frames", DIFFERENT, "RPUs cannot be paired frame by frame (see rpu_present)")
    bad_parse = [(i, r) for i, x, y in pairs for r in (x, y) if not r["ok"]]
    if bad_parse:
        i, r = bad_parse[0]
        return Result(
            "rpu_frames", DIFFERENT, f"{len(bad_parse)} RPUs do not parse (first at display frame {i}: {r['error']})"
        )
    kinds = ("profile", "header", "map_fnv", "dm")
    diff = {k: [] for k in kinds}
    same_bytes = 0
    for i, x, y in pairs:
        same_bytes += x["nal_fnv"] == y["nal_fnv"]
        for k in kinds:
            if x[k] != y[k]:
                diff[k].append(i)
    n = len(pairs)
    label = {
        "profile": "profile",
        "header": "RPU header",
        "map_fnv": "mapping (reshaping) data",
        "dm": "display management data",
    }
    notes = [
        f"{label[k]}: differs on {len(v)} of {n} frames, first at display frame {v[0]}" for k, v in diff.items() if v
    ]
    summary = (
        f"parsed header, mapping and display management data identical on {n - len(set().union(*diff.values()))}"
        f" of {n} frames; raw RPU bytes identical on {same_bytes} of {n}"
    )
    bad = any(diff.values())
    return Result(
        "rpu_frames",
        DIFFERENT if bad else PASS,
        summary,
        notes,
        {"frames": n, "bytes_identical": same_bytes, "differing": {k: len(v) for k, v in diff.items()}},
    )


def ext_blocks(a, b, o):
    if err := _pairing_error("ext_blocks", a, b):
        return err
    if not a["rpus"] and not b["rpus"]:
        return Result("ext_blocks", SKIP, "neither file has Dolby Vision RPUs")
    pairs = _pair(a, b)
    if pairs is None:
        return Result("ext_blocks", DIFFERENT, "RPUs cannot be paired frame by frame (see rpu_present)")
    pairs = [(i, x, y) for i, x, y in pairs if x["ok"] and y["ok"]]
    levels = sorted({int(lv) for _, x, y in pairs for lv in (*x["ext"], *y["ext"])})
    notes, bad, per_level = [], False, {}
    for lv in levels:
        key = str(lv)
        differ, only_in, only_out, first, deltas = 0, 0, 0, None, {}
        for i, x, y in pairs:
            bx, by = x["ext"].get(key), y["ext"].get(key)
            if bx == by:
                continue
            if bx is None:
                only_out += 1
            elif by is None:
                only_in += 1
            else:
                differ += 1
                for sx, sy in zip(bx, by):
                    nx, ny = _numbers(sx), _numbers(sy)
                    for f in nx:
                        if nx[f] != ny.get(f):
                            deltas[f] = max(deltas.get(f, 0), abs(nx[f] - ny.get(f, 0)))
            if first is None:
                first = (i, bx, by)
        total = sum(1 for _, x, _y in pairs if key in x["ext"])
        per_level[f"L{lv}"] = {
            "frames_with_block": total,
            "differ": differ,
            "only_input": only_in,
            "only_output": only_out,
        }
        if differ or only_in or only_out:
            bad = True
            what = [f"{differ} frames differ"] if differ else []
            what += [f"{only_in} lost the block"] if only_in else []
            what += [f"{only_out} gained it"] if only_out else []
            line = f"L{lv}: {', '.join(what)} (of {len(pairs)}); first at display frame {first[0]}"
            if deltas:
                line += "; largest change " + ", ".join(f"{f} {d}" for f, d in sorted(deltas.items()))
            notes.append(line)
            if first[1] and first[2]:
                notes.append(f"   first: {first[1][0]}")
                notes.append(f"      -> {first[2][0]}")
    names = ", ".join(f"L{lv}" for lv in levels)
    summary = (
        f"extension blocks {names}: identical on all {len(pairs)} frames"
        if not bad
        else f"extension blocks {names}: differences found"
    )
    return Result("ext_blocks", DIFFERENT if bad else PASS, summary, notes, {"levels": per_level})


def packets_md5(a, b, o):
    if o.mode != "copy":
        return Result(
            "packets_md5", SKIP, "re-encode mode: packets are expected to differ (use --mode copy for a stream copy)"
        )
    ma, mb = a["md5"], b["md5"]
    return Result("packets_md5", PASS if ma == mb else DIFFERENT, ma if ma == mb else f"{ma} -> {mb}")


def decode(a, b, o):
    (fa, ea), (fb, eb) = a["decode"], b["decode"]
    notes = []
    if ea:
        notes.append(f"input decode messages ({len(ea)}), first: {ea[0]}")
    if eb:
        notes.append(f"output decode messages ({len(eb)}), first: {eb[0]}")
    bad = bool(eb) or fa != fb
    summary = f"decoded {fa} -> {fb} frames, {len(ea)} -> {len(eb)} error messages"
    return Result("decode", DIFFERENT if bad else PASS, summary, notes)


def quality(a, b, o):
    if o.mode != "reencode":
        return Result("quality", SKIP, "stream copy: packets_md5 covers the picture")
    q = b["quality"]
    notes = []
    bad = q["psnr_avg"] < o.min_psnr or q["ssim_all"] < o.min_ssim
    if q["psnr_avg"] < o.min_psnr:
        notes.append(f"PSNR {q['psnr_avg']:.2f} dB is below the {o.min_psnr} dB limit")
    if q["ssim_all"] < o.min_ssim:
        notes.append(f"SSIM {q['ssim_all']:.4f} is below the {o.min_ssim} limit")
    summary = (
        f"frames {o.start}-{o.start + o.frames - 1}: PSNR {q['psnr_avg']:.2f} dB "
        f"(Y {q['psnr_y']:.2f}, worst frame {q['psnr_min']:.2f}), SSIM {q['ssim_all']:.4f}"
    )
    return Result("quality", DIFFERENT if bad else PASS, summary, notes, q)


def gather(tools, path, scratch, tag, want):
    """Everything the checks need about one file. `want` names the checks that will run."""
    side = {"probe": media.probe(tools, path), "packets": media.packets(tools, path)}
    side["ranks"], side["rank_note"] = media.display_ranks(side["packets"])
    side["rpus"] = []
    if want & set(DV_CHECKS[1:]):
        nal = media.extract_rpus(tools, path, f"{scratch}/{tag}.rpu.hevc")
        rows, summary = media.dump_rpus(tools, nal)
        if summary["aus"] != len(side["packets"]):
            side["rpu_error"] = (
                f"{'input' if tag == 'in' else 'output'}: {summary['aus']} access units but "
                f"{len(side['packets'])} packets, so RPUs cannot be attributed to frames (truncated or damaged?)"
            )
        else:
            side["rpus"] = rows
    if "packets_md5" in want:
        side["md5"] = media.video_md5(tools, path)
    if "decode" in want:
        side["decode"] = media.decode(tools, path)
    return side


CHECKS = {
    "container": container,
    "timestamps": timestamps,
    "dv_config": dv_config,
    "rpu_present": rpu_present,
    "rpu_frames": rpu_frames,
    "ext_blocks": ext_blocks,
    "packets_md5": packets_md5,
    "decode": decode,
    "quality": quality,
}
