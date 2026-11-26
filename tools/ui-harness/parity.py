"""Classic parity: the same cells rendered on two refs, bounding boxes of the shared element ids compared."""
import json
from pathlib import Path

import core
from cells import render_cell
from core import EXIT_DIFF, EXIT_NOTHING, EXIT_OK, CellFailure, Report, Session, Site, slug
from states import STATES

# Old theme name -> the name the same palette has in the candidate (spec decision 3).
CLASSIC_ALIAS = {"Default Dark": "Classic Dark", "Default Light": "Classic Light"}
THEMES = ["Default Dark", "Default Light", "Nord", "Jessica Dark"]
EXPLAINED = Path(__file__).resolve().parent / "parity-explained.json"
BOX = ("x", "y", "w", "h")


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


def compare(base, cand, tol):
    """-> list of (id, prop, baseline value, candidate value) for one cell pair."""
    out = []
    be, ce = base["elements"], cand["elements"]
    for i in sorted(set(be) | set(ce)):
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
    return out


def pair_themes(base, cand, rep):
    pairs = []
    for t in THEMES:
        ct = CLASSIC_ALIAS.get(t, t)
        if ct not in cand.theme_names():
            if t in cand.theme_names():
                ct = t
                rep.skip(f"{cand.spec}: no '{CLASSIC_ALIAS[t]}' theme, so '{t}' is used for the candidate")
            else:
                rep.skip(f"{cand.spec}: theme '{ct}' is not in src-tauri/themes, so '{t}' is not compared")
                continue
        if t not in base.theme_names():
            rep.skip(f"{base.spec}: theme '{t}' is not in src-tauri/themes, so it is not compared")
            continue
        pairs.append((t, ct))
    return pairs


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
    explained = load_explained()
    compared = diffs = accepted = 0
    detail = []
    if not (why_b or why_c):
        print(f"parity: {base.spec} ({b_label}) -> {cand.spec} ({c_label}), tolerance {a.tol}px, "
              f"{len(states)} states\n")
        with Site(base) as sb, Site(cand) as sc, Session() as s:
            for bt, ct in pair_themes(base, cand, rep):
                for state in states:
                    where = f"{state} / {bt}" + (f" vs {ct}" if ct != bt else "")
                    try:
                        db = render_cell(s, sb, base, bt, b_cfg, state)
                        dc = render_cell(s, sc, cand, ct, c_cfg, state)
                    except CellFailure as e:
                        rep.fail(where, str(e))
                        continue
                    compared += 1
                    for el, prop, vb, vc in compare(db, dc, a.tol):
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
        {"baseline": a.baseline, "candidate": a.candidate, "tol": a.tol, "compared": compared,
         "differences": detail, "skipped": rep.skips, "failed": rep.failures}, indent=1))
    print(f"\nparity: {compared} cells compared, {diffs} unexplained differences, {accepted} explained, "
          f"{len(rep.failures)} cells failed, {len(rep.skips)} skip notes")
    if compared == 0:
        print("nothing was compared: parity cannot pass")
        return EXIT_NOTHING
    return EXIT_DIFF if (diffs or rep.failures) else EXIT_OK
