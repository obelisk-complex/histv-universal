"""Negative controls: every command is run where it must fail and where it must pass, and the exit
codes are checked. A command that cannot fail proves nothing when it passes."""
import shutil
import subprocess
import sys

import core
import stubs
from core import EXIT_DIFF, EXIT_HARNESS, EXIT_NOTHING, EXIT_OK

HARNESS = str(core.HERE / "harness.py")

# The harness base: the last front end before the style toggle. The controls that need a tree with no
# style control, and the stubs, are built from it; HEAD has the toggle once the branches meet.
NO_CONTROL = "5e73dad"
# The toggle as first built: a live switch changed the stylesheet href at once, before the new
# sheet had loaded. The switch command must fail on it.
SWAP_FIRST = "e955e95"


def run(args, repo=None):
    pre = ["--repo", repo] if repo else []
    p = subprocess.run([sys.executable, HARNESS, *pre, *args], capture_output=True, text=True)
    return p.returncode, (p.stdout + p.stderr).strip().splitlines()


RAIL = "flex: 0 0 var(--rail-w); min-width: 258px; max-width: 460px;"

# name -> (file in src/, edit(text) -> text). Each edit must change the file or the control is void.
def _append(css):
    return lambda t: t + "\n" + css + "\n"


def _replace(old, new):
    def f(t):
        if t.count(old) != 1:
            raise core.HarnessError(f"mutation anchor {old!r} found {t.count(old)} times, expected once")
        return t.replace(old, new)
    return f


MUTATIONS = {
    "unmutated": [],
    "colour": [("css/app-classic.css", _append("#btn-add-files { background-color: #ff00ff !important; }"))],
    "shadow": [("css/app-classic.css", _append("#btn-start { box-shadow: 0 0 0 3px #ff0000 !important; }"))],
    "rail-min": [("css/app-classic.css", _replace(RAIL, RAIL.replace("258px", "300px")))],
    "rail-max": [("css/app-classic.css", _replace(RAIL, RAIL.replace("460px", "400px")))],
}


def mutant(name):
    """Copy of the candidate's front end (the @worktree of --repo) with the named edits -> 'dir:PATH'."""
    src = core.prepare_tree("@worktree")
    dest = core.OUT / "mutants" / name
    shutil.rmtree(dest, ignore_errors=True)
    shutil.copytree(src.root, dest)
    for rel, edit in MUTATIONS[name]:
        f = dest / "src" / rel
        before = f.read_text()
        after = edit(before)
        if after == before:
            raise core.HarnessError(f"mutation {name} did not change {rel}")
        f.write_text(after)
    return f"dir:{dest}"


def parity_args(base, cand):
    return ["parity", "--baseline", base, "--candidate", cand, "--baseline-style", "classic",
            "--candidate-style", "classic", "--map", "Default Dark=Default Dark",
            "--map", "Default Light=Default Light", "--states", "rows", "rail-min", "rail-max"]


def cmd_selftest(a):
    correct = stubs.build_stub("correct", base_ref=NO_CONTROL)
    leaky = stubs.build_stub("leaky", base_ref=NO_CONTROL)
    late = stubs.build_stub("late", base_ref=NO_CONTROL)
    quick = ["--states", "empty", "rows"]
    cases = [
        # (what the control does, args, expected exit code, text the output must contain)
        ("mock: the app asks for a command the mock does not know (get_themes, errors are caught by the app)",
         ["matrix", NO_CONTROL, "--themes", "Default Dark", "--states", "empty", "--mock-drop", "get_themes"],
         EXIT_HARNESS, "does not know invoke get_themes"),
        ("mock: same ref, nothing dropped",
         ["matrix", NO_CONTROL, "--themes", "Default Dark", "--states", "empty"], EXIT_OK, "cells rendered"),
        ("parity: baseline against itself (same mock, two renders), geometry and colours",
         ["parity", "--candidate", "e4d0ee2", "--map", "Default Dark=Default Dark", *quick], EXIT_OK,
         "with colour checks), 0 unexplained", None, "(0 with colour"),
        ("parity: baseline against a tree whose layout differs (the harness base, doublet)",
         ["parity", "--candidate", NO_CONTROL, "--candidate-style", "native", *quick], EXIT_DIFF, "DIFF"),
        ("parity: classic requested on a tree with no style control",
         ["parity", "--candidate", NO_CONTROL, "--states", "empty"], EXIT_NOTHING, "nothing was compared"),
        ("toggle: correct stub",
         ["toggle", correct, "--themes", "Default Dark", "--cycles", "2"] + quick, EXIT_OK, "0 failed"),
        ("toggle: stub that leaves the other style's custom properties on <html>",
         ["toggle", leaky, "--themes", "Default Dark", "--cycles", "1", "--max-lines", "3"] + quick, EXIT_DIFF,
         "custom properties of doublet remain on <html>"),
        ("toggle: tree with no style control",
         ["toggle", NO_CONTROL], EXIT_NOTHING, "nothing was switched"),
        ("paint: correct stub (style applied in <head>)", ["paint", correct], EXIT_OK, "0 failed"),
        ("paint: stub that applies the style after get_config answers",
         ["paint", late], EXIT_DIFF, "painted frames show the wrong style"),
        ("paint: tree with no style control", ["paint", NO_CONTROL], EXIT_NOTHING, "nothing was checked"),
        ("switch: tree with no style control", ["switch", NO_CONTROL], EXIT_NOTHING, "nothing was switched"),
    ]
    skipped = []
    if a.candidate_repo:
        repo = str(core.Path(a.candidate_repo).resolve())
        core.REPO = core.Path(repo)
        base = mutant("unmutated")
        for name, expect in (("colour", "#btn-add-files  background-color"), ("shadow", "#btn-start  box-shadow"),
                             ("rail-min", "#right-panel  w: baseline 258  candidate 300"),
                             ("rail-max", "#right-panel  w: baseline 460  candidate 400")):
            cases.append((f"parity on the candidate's front end, mutated: {name}",
                          parity_args(base, mutant(name)), EXIT_DIFF, expect, repo))
        cases.append(("parity on the candidate's front end, unmutated copy (colour and rail states)",
                      parity_args(base, mutant("unmutated")), EXIT_OK, "with colour checks), 0 unexplained", repo, "(0 with colour"))
        cases.append(("switch: candidate at e955e95, which swapped the stylesheets before the new ones had loaded",
                      ["switch", SWAP_FIRST, "--themes", "Default Dark"], EXIT_DIFF,
                      "frames show neither style fully applied", repo))
        cases.append(("switch: the candidate's front end, unmutated copy",
                      ["switch", base, "--themes", "Default Dark"], EXIT_OK, "switches checked", repo, "FAIL"))
    else:
        skipped = ["colour, shadow, rail-min and rail-max mutations need --candidate-repo PATH "
                   "(a worktree whose front end has the style toggle)"]
    bad = 0
    for case in cases:
        what, args, want, text = case[:4]
        repo_arg = case[4] if len(case) > 4 else None
        forbid = case[5] if len(case) > 5 else None   # text that must not appear (a check that did not run)
        code, out = run(args, repo_arg)
        found = any(text in ln for ln in out) and not (forbid and any(forbid in ln for ln in out))
        ok = code == want and found
        bad += not ok
        print(f"{'ok  ' if ok else 'FAIL'}  exit {code} (want {want}){'' if found else f', output check failed (wanted {text!r})'}  {what}",
              flush=True)
        if not ok:
            print("\n".join("        " + ln for ln in out[-6:]))
    for sk in skipped:
        print(f"SKIP  {sk}")
    print(f"\nselftest: {len(cases) - bad}/{len(cases)} controls behaved as expected"
          + (f", {len(skipped)} skipped" if skipped else ""))
    if bad:
        return EXIT_DIFF
    return EXIT_NOTHING if skipped else EXIT_OK
