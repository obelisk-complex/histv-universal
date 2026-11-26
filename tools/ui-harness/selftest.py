"""Negative controls: every command is run where it must fail and where it must pass, and the exit
codes are checked. A command that cannot fail proves nothing when it passes."""
import subprocess
import sys

import core
import stubs
from core import EXIT_DIFF, EXIT_HARNESS, EXIT_NOTHING, EXIT_OK

HARNESS = str(core.HERE / "harness.py")


def run(args):
    p = subprocess.run([sys.executable, HARNESS, *args], capture_output=True, text=True)
    return p.returncode, (p.stdout + p.stderr).strip().splitlines()


def cmd_selftest(a):
    correct = stubs.build_stub("correct")
    leaky = stubs.build_stub("leaky")
    late = stubs.build_stub("late")
    quick = ["--states", "empty", "rows"]
    cases = [
        # (what the control does, args, expected exit code, text the output must contain)
        ("mock: the app asks for a command the mock does not know (get_themes, errors are caught by the app)",
         ["matrix", "HEAD", "--themes", "Default Dark", "--states", "empty", "--mock-drop", "get_themes"],
         EXIT_HARNESS, "does not know invoke get_themes"),
        ("mock: same ref, nothing dropped",
         ["matrix", "HEAD", "--themes", "Default Dark", "--states", "empty"], EXIT_OK, "cells rendered"),
        ("parity: baseline against itself (same mock, two renders)",
         ["parity", "--candidate", "e4d0ee2", *quick], EXIT_OK, "0 unexplained differences"),
        ("parity: baseline against a tree whose layout differs (HEAD as it renders now, doublet)",
         ["parity", "--candidate", "HEAD", "--candidate-style", "native", *quick], EXIT_DIFF, "DIFF"),
        ("parity: classic requested on a tree with no style control",
         ["parity", "--candidate", "HEAD", "--states", "empty"], EXIT_NOTHING, "nothing was compared"),
        ("toggle: correct stub",
         ["toggle", correct, "--themes", "Default Dark", "--cycles", "2"] + quick, EXIT_OK, "0 failed"),
        ("toggle: stub that leaves the other style's custom properties on <html>",
         ["toggle", leaky, "--themes", "Default Dark", "--cycles", "1", "--max-lines", "3"] + quick, EXIT_DIFF,
         "custom properties of doublet remain on <html>"),
        ("toggle: tree with no style control",
         ["toggle", "HEAD"], EXIT_NOTHING, "nothing was switched"),
        ("paint: correct stub (style applied in <head>)", ["paint", correct], EXIT_OK, "0 failed"),
        ("paint: stub that applies the style after get_config answers",
         ["paint", late], EXIT_DIFF, "painted frames show the wrong style"),
        ("paint: tree with no style control", ["paint", "HEAD"], EXIT_NOTHING, "nothing was checked"),
    ]
    bad = 0
    for what, args, want, text in cases:
        code, out = run(args)
        found = any(text in ln for ln in out)
        ok = code == want and found
        bad += not ok
        print(f"{'ok  ' if ok else 'FAIL'}  exit {code} (want {want}){'' if found else f', output lacks {text!r}'}  {what}",
              flush=True)
        if not ok:
            print("\n".join("        " + ln for ln in out[-6:]))
    print(f"\nselftest: {len(cases) - bad}/{len(cases)} controls behaved as expected")
    return EXIT_DIFF if bad else EXIT_OK
