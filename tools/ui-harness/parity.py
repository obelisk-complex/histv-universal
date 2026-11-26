"""Classic parity: the same cells rendered on two refs, bounding boxes of the shared element ids compared."""
import json
import re
from pathlib import Path

import core
from cells import render_cell
from core import EXIT_DIFF, EXIT_NOTHING, EXIT_OK, CellFailure, Report, Session, Site, slug
from states import STATES

# Old theme name -> the name the same palette has in the candidate (spec decision 3: the implementer
# built Classic Dark and Classic Light from the old Default Dark and Default Light values). Only
# these pairs get colour checks; the other themes changed palette on purpose since 2.6.0.
DEFAULT_MAP = {"Default Dark": "Classic Dark", "Default Light": "Classic Light"}
THEMES = ["Default Dark", "Default Light", "Nord", "Jessica Dark"]
COLOUR_PROPS = {"color": "color", "bg": "background-color", "borderTop": "border-top-color",
                "borderBottom": "border-bottom-color", "shadow": "box-shadow"}
EXPLAINED = Path(__file__).resolve().parent / "parity-explained.json"
BOX = ("x", "y", "w", "h")
# Ids the contract adds on top of the old markup; they have no counterpart to compare with.
CONTRACT_IDS = {"sel-ui-style", "ui-style-css", "doublet-tokens-css", "doublet-components-css"}


def load_explained():
    """[{id, reason, states?: [...], props?: [...]}]: differences accepted because the new wording
    changes the text size. Each entry names the id and says why; nothing is accepted by default."""
    if not EXPLAINED.exists():
        return []
    return json.loads(EXPLAINED.read_text())


def explained_by(entries, state, el_id, prop):
    for e in entries:
        if e["id"] == el_id and state in e.get("states", [state]) and prop in e.get("props", [prop]):
            return e["reason"]
    return None


_TOKEN = re.compile(r"rgba?\([^)]*\)|-?\d*\.?\d+px|\S+")


def _channels(tok):
    nums = [float(x) for x in re.findall(r"-?\d*\.?\d+", tok)]
    return nums + [1.0] * (4 - len(nums)) if len(nums) == 3 else nums


def colours_close(a, b, ctol, tol):
    """Computed colour or box-shadow strings. Colours: every channel within ctol (0-255), alpha within
    0.02. Lengths inside a shadow: within tol px. Anything else in the string must be identical."""
    if a == b:
        return True
    ta, tb = _TOKEN.findall(a), _TOKEN.findall(b)
    if len(ta) != len(tb):
        return False
    for x, y in zip(ta, tb):
        if x == y:
            continue
        if x.startswith("rgb") and y.startswith("rgb"):
            ca, cb = _channels(x), _channels(y)
            if len(ca) != len(cb) or any(abs(i - j) > ctol for i, j in zip(ca[:3], cb[:3])) \
                    or abs(ca[3] - cb[3]) > 0.02:
                return False
        elif x.endswith("px") and y.endswith("px"):
            if abs(float(x[:-2]) - float(y[:-2])) > tol:
                return False
        else:
            return False
    return True


def compare(base, cand, tol, colour=False, ctol=2):
    """-> list of (id, prop, baseline value, candidate value) for one cell pair.
    colour=True also compares the computed colours of elements shown on both sides."""
    out = []
    be, ce = base["elements"], cand["elements"]
    for i in sorted((set(be) | set(ce)) - CONTRACT_IDS):
        if i not in ce:
            out.append((i, "present", "yes", "no"))
            continue
        if i not in be:
            out.append((i, "present", "no", "yes"))
            continue
        for prop in ("display", "visibility"):
            if be[i][prop] != ce[i][prop]:
                out.append((i, prop, be[i][prop], ce[i][prop]))
        for prop in BOX:
            if abs(be[i][prop] - ce[i][prop]) > tol:
                out.append((i, prop, be[i][prop], ce[i][prop]))
        if colour and be[i]["display"] != "none" and ce[i]["display"] != "none":
            for key, name in COLOUR_PROPS.items():
                if not colours_close(be[i][key], ce[i][key], ctol, tol):
                    out.append((i, name, be[i][key], ce[i][key]))
    return out


def pair_themes(base, cand, rep, mapping):
    """-> [(baseline theme, candidate theme, colour check?)]. A theme in `mapping` whose candidate name
    exists is compared for geometry and colour; any other theme is compared for geometry only."""
    pairs = []
    for t in THEMES:
        if t not in base.theme_names():
            rep.skip(f"{base.spec}: theme '{t}' is not in src-tauri/themes, so it is not compared")
            continue
        if t in mapping and mapping[t] in cand.theme_names():
            pairs.append((t, mapping[t], True))
        elif t in cand.theme_names():
            if t in mapping:
                rep.skip(f"{cand.spec}: no '{mapping[t]}' theme, so '{t}' is compared for geometry only (no colour check)")
            pairs.append((t, t, False))
        else:
            rep.skip(f"{cand.spec}: theme '{t}' is not in src-tauri/themes, so it is not compared")
    return pairs


def parse_map(items):
    if items is None:
        return dict(DEFAULT_MAP)
    out = {}
    for it in items:
        old, sep, new = it.partition("=")
        if not sep or not old or not new:
            raise core.HarnessError(f"--map expects OLD=NEW, got {it!r}")
        out[old] = new
    return out


def cmd_parity(a):
    base = core.prepare_tree(a.baseline)
    cand = core.prepare_tree(a.candidate)
    rep = Report()
    b_label, b_cfg, why_b = core.resolve_style(base, a.baseline_style)
    c_label, c_cfg, why_c = core.resolve_style(cand, a.candidate_style)
    for why in (why_b, why_c):
        if why:
            rep.skip(why)
    states = a.states or list(STATES)
    mapping = parse_map(a.map)
    explained = load_explained()
    compared = coloured = diffs = accepted = 0
    detail = []
    if not (why_b or why_c):
        print(f"parity: {base.spec} ({b_label}) -> {cand.spec} ({c_label}), tolerance {a.tol}px, "
              f"{len(states)} states\n")
        with Site(base) as sb, Site(cand) as sc, Session() as s:
            for bt, ct, colour in pair_themes(base, cand, rep, mapping):
                colour = colour and b_label == c_label
                for state in states:
                    where = f"{state} / {bt}" + (f" vs {ct}" if ct != bt else "")
                    try:
                        db = render_cell(s, sb, base, bt, b_cfg, state)
                        dc = render_cell(s, sc, cand, ct, c_cfg, state)
                    except CellFailure as e:
                        rep.fail(where, str(e))
                        continue
                    compared += 1
                    coloured += colour
                    for el, prop, vb, vc in compare(db, dc, a.tol, colour, a.colour_tol):
                        why = explained_by(explained, state, el, prop)
                        d = {"cell": where, "id": el, "prop": prop, "baseline": vb, "candidate": vc, "explained": why}
                        detail.append(d)
                        if why:
                            accepted += 1
                            print(f"EXPLAINED {where}  #{el}  {prop}: {vb} -> {vc}  ({why})")
                        else:
                            diffs += 1
                            delta = f"  (diff {round(vc - vb, 2)})" if isinstance(vb, (int, float)) else ""
                            print(f"DIFF  {where}  #{el}  {prop}: baseline {vb}  candidate {vc}{delta}")
    out = core.OUT / "parity"
    out.mkdir(parents=True, exist_ok=True)
    (out / f"{slug(base.label)}__{slug(cand.label)}.json").write_text(json.dumps(
        {"baseline": a.baseline, "candidate": a.candidate, "tol": a.tol, "compared": compared, "colour_checked": coloured, "map": mapping, "colour_tol": a.colour_tol,
         "differences": detail, "skipped": rep.skips, "failed": rep.failures}, indent=1))
    by_id = {}
    for d in detail:
        if not d["explained"]:
            by_id.setdefault(d["id"], set()).add(d["cell"].split(" / ")[0])
    if by_id:
        print("\nunexplained differences by id (id: states):")
        for i, st in sorted(by_id.items(), key=lambda kv: (-len(kv[1]), kv[0])):
            print(f"  #{i}: {', '.join(sorted(st))}")
    print(f"\nparity: {compared} cells compared ({coloured} with colour checks), {diffs} unexplained differences, {accepted} explained, "
          f"{len(rep.failures)} cells failed, {len(rep.skips)} skip notes")
    if compared == 0:
        print("nothing was compared: parity cannot pass")
        return EXIT_NOTHING
    return EXIT_DIFF if (diffs or rep.failures) else EXIT_OK
