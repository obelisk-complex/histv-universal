"""Shared plumbing: trees of a git ref, a local static server, a headless Chromium session, geometry dumps."""
import functools
import http.server
import json
import os
import re
import shutil
import subprocess
import threading
from dataclasses import dataclass, field
from pathlib import Path

import fixtures

HERE = Path(__file__).resolve().parent
REPO = Path(subprocess.check_output(["git", "-C", str(HERE), "rev-parse", "--show-toplevel"], text=True).strip())
OUT = Path(os.environ.get("UI_HARNESS_OUT") or (Path(os.environ.get("TMPDIR", "/tmp")) / "histv-ui-harness"))
CHROMIUM = os.environ.get("UI_HARNESS_CHROMIUM", "/usr/bin/chromium")

# Exit codes shared by every command.
EXIT_OK, EXIT_DIFF, EXIT_NOTHING, EXIT_HARNESS = 0, 1, 2, 3

VIEWPORT = (1280, 800)
NARROW = (800, 900)

# Zero-length transitions and animations: geometry and computed styles are read
# at their end state instead of mid-flight. Not used by the first-paint check.
KILL_MOTION_CSS = (
    "*,*::before,*::after{transition-duration:0s!important;transition-delay:0s!important;"
    "animation-duration:0s!important;animation-delay:0s!important;"
    "animation-iteration-count:1!important;scroll-behavior:auto!important}"
)


# Installed before any page script runs, so that no transition starts before the rule applies.
KILL_MOTION = (
    "(() => { const css = %s; const put = () => { if (!document.head) return false;"
    " const st = document.createElement('style'); st.id = 'uih-kill-motion'; st.textContent = css;"
    " document.head.appendChild(st); return true; };"
    " if (put()) return; const mo = new MutationObserver(() => { if (put()) mo.disconnect(); });"
    " mo.observe(document, { childList: true, subtree: true }); })();"
) % json.dumps(KILL_MOTION_CSS)


class HarnessError(Exception):
    """The harness or the mock could not do its job (unknown bridge call, page error). Exit 3."""


class CellFailure(Exception):
    """The page loaded but is not in the state the cell asked for. A finding, not a crash. Exit 1."""


def slug(text):
    return re.sub(r"[^A-Za-z0-9._-]+", "_", text).strip("_")


# ── Trees ────────────────────────────────────────────────────────────────────


@dataclass
class Tree:
    spec: str
    label: str
    root: Path
    themes: list
    has_control: bool
    native_style: str
    notes: list = field(default_factory=list)

    @property
    def src(self):
        return self.root / "src"

    def theme_names(self):
        return [t["name"] for t in self.themes]


def prepare_tree(spec, repo=None):
    """spec: a git ref, '@worktree' (the repo's working tree, uncommitted changes included),
    or 'dir:PATH' (a folder that already holds src/ and src-tauri/themes/)."""
    repo = Path(repo or REPO)
    notes = []
    if spec.startswith("dir:"):
        root = Path(spec[4:]).resolve()
        label = "dir-" + slug(root.name)
    elif spec == "@worktree":
        root = OUT / "trees" / "worktree"
        shutil.rmtree(root, ignore_errors=True)
        (root / "src-tauri").mkdir(parents=True)
        shutil.copytree(repo / "src", root / "src")
        if (repo / "src-tauri" / "themes").is_dir():
            shutil.copytree(repo / "src-tauri" / "themes", root / "src-tauri" / "themes")
        label = "worktree"
    else:
        sha = subprocess.check_output(["git", "-C", str(repo), "rev-parse", "--verify", f"{spec}^{{commit}}"], text=True).strip()
        root = OUT / "trees" / sha[:12]
        label = f"{slug(spec)}-{sha[:7]}"
        if not (root / "src" / "index.html").exists():
            shutil.rmtree(root, ignore_errors=True)
            root.mkdir(parents=True)
            paths = ["src"]
            has_themes = subprocess.run(["git", "-C", str(repo), "cat-file", "-e", f"{sha}:src-tauri/themes"],
                                        capture_output=True).returncode == 0
            if has_themes:
                paths.append("src-tauri/themes")
            else:
                notes.append(f"{spec} has no src-tauri/themes; the mock serves no themes")
            arc = subprocess.Popen(["git", "-C", str(repo), "archive", sha, *paths], stdout=subprocess.PIPE)
            subprocess.run(["tar", "-x", "-C", str(root)], stdin=arc.stdout, check=True)
            arc.stdout.close()
            if arc.wait() != 0:
                raise HarnessError(f"git archive {spec} failed")
    index = (root / "src" / "index.html").read_text()
    return Tree(
        spec=spec,
        label=label,
        root=root,
        themes=fixtures.themes_from_tree(root),
        has_control='id="sel-ui-style"' in index,
        native_style="doublet" if (root / "src" / "css" / "doublet-tokens.css").exists() else "classic",
        notes=notes,
    )


def resolve_style(tree, style):
    """-> (label, uiStyle to persist or None, skip reason or None).
    'native' means whatever the tree renders without being told."""
    if style == "native":
        return tree.native_style, None, None
    if tree.has_control:
        return style, style, None
    if style == tree.native_style:
        return style, None, None
    return style, None, (f"{tree.spec}: no #sel-ui-style and it renders {tree.native_style} natively, "
                         f"so {style} cells are skipped")


def theme_skip(tree, theme):
    if theme not in tree.theme_names():
        return f"{tree.spec}: theme '{theme}' is not in src-tauri/themes, so its cells are skipped"
    return None


# ── Static server ────────────────────────────────────────────────────────────


class _Handler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def end_headers(self):
        self.send_header("Cache-Control", "no-store")
        super().end_headers()


class Site:
    def __init__(self, tree):
        handler = functools.partial(_Handler, directory=str(tree.src))
        self.httpd = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
        self.url = f"http://127.0.0.1:{self.httpd.server_address[1]}/"
        self.thread = threading.Thread(target=self.httpd.serve_forever, daemon=True)

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *a):
        self.httpd.shutdown()
        self.httpd.server_close()


# ── Browser session ──────────────────────────────────────────────────────────


class Session:
    """One headless Chromium. No display server is reachable from it and its
    profile and config directories live under OUT, never under the real home."""

    def __enter__(self):
        from playwright.sync_api import sync_playwright
        home = OUT / "home"
        home.mkdir(parents=True, exist_ok=True)
        env = {k: v for k, v in os.environ.items() if k not in ("DISPLAY", "WAYLAND_DISPLAY")}
        env.update(HOME=str(home), XDG_CONFIG_HOME=str(home / ".config"), XDG_CACHE_HOME=str(home / ".cache"),
                   XDG_DATA_HOME=str(home / ".local" / "share"))
        self.pw = sync_playwright().start()
        self.browser = self.pw.chromium.launch(executable_path=CHROMIUM, headless=True, env=env)
        return self

    def __exit__(self, *a):
        self.browser.close()
        self.pw.stop()


class Loaded:
    def __init__(self, context, page, errors):
        self.context, self.page, self.errors = context, page, errors

    def close(self):
        if self.context:
            self.context.close()

    def check(self):
        """Raise HarnessError if the mock was asked something it does not know,
        or the page threw. Called after every step that touches the page."""
        failures = self.page.evaluate("window.__MOCK__ ? window.__MOCK__.failures : [{kind:'mock', name:'missing'}]")
        problems = [f"the mock does not know {f['kind']} {f['name']}" for f in failures] + list(self.errors)
        if problems:
            raise HarnessError("; ".join(problems))


def _is_ours(url, base):
    return url.startswith(base) or url.startswith("data:") or url.startswith("blob:") or url.startswith("about:")


def open_page(session, site, tree, theme, ui_style=None, viewport=VIEWPORT, delays=None, drop=None,
              fake_clock=True, kill_motion=True, context=None, seed_js=None, wait=True):
    """Fresh context (or the one given), mock injected, page loaded and settled."""
    own = context is None
    ctx = context or session.browser.new_context(viewport={"width": viewport[0], "height": viewport[1]},
                                                 device_scale_factor=1)
    errors = []
    fx = fixtures.build(tree.themes, theme, ui_style, delays, drop, fake_clock)
    page = ctx.new_page()
    page.on("pageerror", lambda e: errors.append(f"pageerror: {e}"))

    def gate(route):
        if _is_ours(route.request.url, site.url):
            route.continue_()
        else:
            errors.append(f"external request blocked: {route.request.url}")
            route.abort()
    page.route("**/*", gate)
    if kill_motion:
        page.add_init_script(KILL_MOTION)
    if seed_js:
        page.add_init_script(seed_js)
    page.add_init_script("window.__MOCK_FIXTURES__ = " + json.dumps(fx) + ";")
    page.add_init_script(path=str(HERE / "mock-tauri.js"))
    page.goto(site.url, wait_until="load")
    loaded = Loaded(ctx if own else None, page, errors)
    if wait:
        wait_ready(page)
        loaded.check()
    return loaded


def raf2(page, extra_ms=60):
    page.evaluate("new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)))")
    page.wait_for_timeout(extra_ms)


def wait_ready(page, timeout=10000):
    """Start-up is done once the encoder list has been fetched (the last call of the
    DOMContentLoaded handler's chain), then two frames for the layout to settle."""
    try:
        page.wait_for_function("window.__MOCK__ && (window.__MOCK__.settled.get_detected_encoders || 0) >= 1",
                               timeout=timeout)
    except Exception as e:
        calls = page.evaluate("window.__MOCK__ ? window.__MOCK__.calls.map(c => c.kind + ':' + c.name) : null")
        raise HarnessError(f"start-up did not finish (calls so far: {calls}): {e}")
    raf2(page)


def check_applied(page, theme, style, tree):
    """The page must really be in the theme and style the cell asked for; otherwise a
    fallback to the default would be mistaken for a render of the requested cell."""
    got = page.evaluate("""() => ({
        theme: document.querySelector('#sel-theme').value,
        select: document.querySelector('#sel-ui-style') ? document.querySelector('#sel-ui-style').value : null,
        marker: document.documentElement.dataset.uiStyle || null })""")
    if got["theme"] != theme:
        raise CellFailure(f"theme is '{got['theme']}', expected '{theme}'")
    if tree.has_control and style:
        if got["select"] != style or got["marker"] != style:
            raise CellFailure(f"style control '{got['select']}' / data-ui-style '{got['marker']}', expected '{style}'")
    return got


DUMP_JS = """() => {
  const r2 = (v) => Math.round(v * 100) / 100;
  const out = {}, dup = [];
  for (const e of document.querySelectorAll('[id]')) {
    if (Object.prototype.hasOwnProperty.call(out, e.id)) { dup.push(e.id); continue; }
    const b = e.getBoundingClientRect(), cs = getComputedStyle(e);
    out[e.id] = { x: r2(b.x), y: r2(b.y), w: r2(b.width), h: r2(b.height), display: cs.display, visibility: cs.visibility };
  }
  const link = document.getElementById('ui-style-css');
  return {
    elements: out, duplicates: dup,
    meta: {
      uiStyle: document.documentElement.dataset.uiStyle || null,
      styleLink: link ? link.getAttribute('href') : null,
      sheets: [...document.styleSheets].map(s => ({ href: s.href ? s.href.split('/').slice(-2).join('/') : null, disabled: s.disabled })),
      viewport: { w: innerWidth, h: innerHeight },
    },
  };
}"""


def dump(page):
    return page.evaluate(DUMP_JS)


class Report:
    """Collects skips and failures so a run ends with every note printed once."""

    def __init__(self):
        self.skips, self.failures, self.rendered = [], [], 0

    def skip(self, reason):
        if reason not in self.skips:
            self.skips.append(reason)
            print(f"SKIP  {reason}", flush=True)

    def fail(self, where, why):
        self.failures.append((where, why))
        print(f"FAIL  {where}: {why}", flush=True)
