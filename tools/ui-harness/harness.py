#!/usr/bin/env python3
"""Entry point: harness.py matrix|parity|toggle|paint ... (run through run.sh)."""
import argparse
import json
import sys

import core
from core import EXIT_DIFF, EXIT_HARNESS, EXIT_NOTHING, EXIT_OK, CellFailure, HarnessError, Report, Session, Site, slug
from cells import render_cell
from paint import cmd_paint
from parity import cmd_parity
from selftest import cmd_selftest
from toggle import cmd_toggle
from states import STATES, STYLES, THEMES


def cmd_matrix(a):
    tree = core.prepare_tree(a.ref)
    for n in tree.notes:
        print(f"NOTE  {n}")
    styles = a.styles or STYLES
    themes = a.themes or THEMES
    states = a.states or list(STATES)
    rep = Report()
    outdir = core.OUT / "matrix" / tree.label
    index = []
    with Site(tree) as site, Session() as s:
        for style in styles:
            label, ui_style, why = core.resolve_style(tree, style)
            if why:
                rep.skip(why)
                continue
            for theme in themes:
                why = core.theme_skip(tree, theme)
                if why:
                    rep.skip(why)
                    continue
                for state in states:
                    cell = f"{tree.label}/{label}/{theme}/{state}"
                    base = outdir / label / slug(theme) / state
                    try:
                        d = render_cell(s, site, tree, theme, ui_style, state, shot_path=base.with_suffix(".png"),
                                        drop=a.mock_drop)
                    except CellFailure as e:
                        rep.fail(cell, str(e))
                        continue
                    base.with_suffix(".json").write_text(json.dumps(d, indent=1, sort_keys=True))
                    rep.rendered += 1
                    index.append(cell)
    (outdir).mkdir(parents=True, exist_ok=True)
    (outdir / "summary.json").write_text(json.dumps(
        {"ref": a.ref, "rendered": index, "skipped": rep.skips, "failed": rep.failures}, indent=1))
    print(f"\nmatrix {tree.label}: {rep.rendered} cells rendered, {len(rep.skips)} skip notes, "
          f"{len(rep.failures)} failed; output in {outdir}")
    if rep.rendered == 0:
        print("nothing was rendered")
        return EXIT_NOTHING
    return EXIT_DIFF if rep.failures else EXIT_OK


def main():
    p = argparse.ArgumentParser(prog="run.sh")
    sub = p.add_subparsers(dest="cmd", required=True)
    m = sub.add_parser("matrix", help="render the state matrix of one ref")
    m.add_argument("ref", help="git ref, @worktree, or dir:PATH")
    m.add_argument("--styles", nargs="*")
    m.add_argument("--themes", nargs="*")
    m.add_argument("--states", nargs="*", choices=list(STATES))
    m.add_argument("--mock-drop", nargs="*", default=[], metavar="CMD",
                   help="negative control: make the mock not know these commands")
    m.set_defaults(fn=cmd_matrix)
    q = sub.add_parser("parity", help="compare bounding boxes of shared ids between two refs")
    q.add_argument("--baseline", default="e4d0ee2", help="default: the last commit before Doublet")
    q.add_argument("--candidate", default="HEAD")
    q.add_argument("--baseline-style", default="native", choices=["native", "doublet", "classic"])
    q.add_argument("--candidate-style", default="classic", choices=["native", "doublet", "classic"])
    q.add_argument("--tol", type=float, default=1.0, help="allowed difference per x, y, w, h in px (default 1)")
    q.add_argument("--states", nargs="*", choices=list(STATES))
    q.set_defaults(fn=cmd_parity)
    t = sub.add_parser("toggle", help="live style switches against fresh loads")
    t.add_argument("ref", nargs="?", default="@worktree", help="git ref, @worktree (default) or dir:PATH")
    t.add_argument("--themes", nargs="*")
    t.add_argument("--states", nargs="*", choices=list(STATES))
    t.add_argument("--cycles", type=int, default=3, help="doublet -> classic -> doublet rounds (default 3)")
    t.add_argument("--max-lines", type=int, default=12, help="differences printed per failing switch")
    t.set_defaults(fn=cmd_toggle)
    n = sub.add_parser("paint", help="persisted style applied in the first painted frame")
    n.add_argument("ref", nargs="?", default="@worktree", help="git ref, @worktree (default) or dir:PATH")
    n.add_argument("--theme", default="Default Dark")
    n.add_argument("--styles", nargs="*", default=["classic", "doublet"], choices=["classic", "doublet"])
    n.add_argument("--ipc-delay", type=int, default=300, metavar="MS",
                   help="how long get_config and get_themes take to answer (default 300)")
    n.add_argument("--seed", default="", metavar="JS",
                   help="script run before the page's own in the watched load, for an early channel the harness cannot "
                        "learn by itself (e.g. \"window.__HISTV_UI_STYLE__='classic'\")")
    n.set_defaults(fn=cmd_paint)
    c = sub.add_parser("selftest", help="negative controls: each command must fail where it should")
    c.set_defaults(fn=cmd_selftest)
    a = p.parse_args()
    try:
        return a.fn(a)
    except HarnessError as e:
        print(f"HARNESS ERROR: {e}", file=sys.stderr)
        return EXIT_HARNESS


if __name__ == "__main__":
    sys.exit(main())
