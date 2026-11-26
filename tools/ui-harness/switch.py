"""Live switch with slow stylesheets: every frame shows one style, fully applied, and a stylesheet
that fails to load leaves the old style in place."""
import core
from core import EXIT_DIFF, EXIT_NOTHING, EXIT_OK, CellFailure, Report, Session, Site, check_applied, open_page, raf2

# The stylesheets each style has applied, by file name (the contract in the spec).
SHEETS = {"doublet": "app.css|doublet-components.css|doublet-tokens.css", "classic": "app-classic.css"}
CSS = ["css/app.css", "css/app-classic.css", "css/doublet-tokens.css", "css/doublet-components.css"]
OTHER = {"doublet": "classic", "classic": "doublet"}

# One sample per animation frame (see paint.py): the marker, the stylesheets that apply (loaded,
# enabled and with matching media) and the custom properties set inline on <html> by applyTheme.
SAMPLER_JS = """() => {
  const sig = () => {
    const h = document.documentElement;
    const applies = (s) => s.href && !s.disabled && (!s.media.length || matchMedia(s.media.mediaText).matches);
    // A stylesheet answered with an error can still be listed, with no rules: marked, so it does not
    // pass for the real one.
    const sheets = [...document.styleSheets].filter(applies)
      .map(s => s.href.split('/').pop() + (s.cssRules.length ? '' : '(empty)')).sort().join('|');
    const props = [];
    for (let i = 0; i < h.style.length; i++) if (h.style[i].startsWith('--')) props.push(h.style[i]);
    return { marker: h.dataset.uiStyle || null, sheets, props: props.sort().join(' '),
             pairing: h.getAttribute('data-dx-pairing') };
  };
  window.__SIG__ = sig;
  window.__FRAMES__ = [];
  window.__FRAMES_STOP__ = false;
  const loop = (t) => {
    window.__FRAMES__.push(Object.assign({ t }, sig()));
    if (!window.__FRAMES_STOP__ && window.__FRAMES__.length < 3000) requestAnimationFrame(loop);
  };
  requestAnimationFrame(loop);
}"""

KEYS = ("marker", "sheets", "props", "pairing")


def key(f):
    return tuple(f[k] for k in KEYS)


def describe(f):
    dx = sum(1 for p in f["props"].split() if p.startswith("--dx-"))
    other = len(f["props"].split()) - dx
    return (f"data-ui-style={f['marker']}, sheets applied=[{f['sheets']}], "
            f"inline custom properties: {dx} --dx-*, {other} other")


def judge(frames, before, after):
    """Every frame from the switch on is the old page or the new page, and once new never old
    again. -> (ok, message)."""
    seen_new = False
    held = 0
    for f in frames:
        k = key(f)
        if k == key(after) and k != key(before):
            seen_new = True
        elif k == key(before):
            if seen_new:
                return False, f"frame at {f['t']:.0f} ms went back to the old style after the new one showed"
            held += 1
        else:
            bad = [g for g in frames if key(g) not in (key(before), key(after))]
            return False, (f"{len(bad)} frames show neither style fully applied; the first, at {f['t']:.0f} ms, "
                           f"shows {describe(f)}")
    return True, f"{len(frames)} frames, {held} of them in the old style while the stylesheets loaded"


def one_switch(page, site, old, new, delay, fail):
    """Switch through the control with the stylesheets held (or failing). -> (ok, message)."""
    before = page.evaluate("window.__SIG__()")
    if before["marker"] != old or before["sheets"] != SHEETS[old]:
        raise CellFailure(f"before the switch the page is not in {old}: {describe(before)}")
    site.hold(CSS, delay, fail)
    try:
        at = page.evaluate("window.__FRAMES__.length")
        page.select_option("#sel-ui-style", new)
        if fail:
            page.wait_for_timeout(delay + 1000)
        else:
            try:
                page.wait_for_function(
                    "([m, s]) => { const f = window.__SIG__(); return f.marker === m && f.sheets === s; }",
                    arg=[new, SHEETS[new]], timeout=delay * 4 + 5000)
            except Exception:
                return False, f"the switch to {new} did not complete: {describe(page.evaluate('window.__SIG__()'))}"
        raf2(page, 150)
    finally:
        site.hold()
    after = page.evaluate("window.__SIG__()")
    frames = page.evaluate("(i) => window.__FRAMES__.slice(i)", at)
    want = old if fail else new
    if after["marker"] != want or after["sheets"] != SHEETS[want]:
        return False, f"the settled page is not in {want}: {describe(after)}"
    if fail:
        control = page.evaluate("document.querySelector('#sel-ui-style').value")
        if control != old:
            return False, f"the style kept {old} but the control shows {control}"
        if key(after) != key(before):
            return False, f"the settled page differs from the page before the failed switch: {describe(after)}"
    return judge(frames, before, after)


def cmd_switch(a):
    tree = core.prepare_tree(a.ref)
    rep = Report()
    if not tree.has_control:
        rep.skip(f"{tree.spec} has no #sel-ui-style: nothing to switch")
        print("\nswitch: nothing was switched, so it cannot pass")
        return EXIT_NOTHING
    # (start style, [(target, file that fails or None), ...]). Starting in classic from the cache
    # means the Doublet sheets were never fetched; that is the slowest switch.
    runs = [
        ("doublet", [("classic", None), ("doublet", None), ("classic", None)]),
        ("classic", [("doublet", None), ("classic", None)]),
        ("doublet", [("classic", "css/app-classic.css"), ("classic", None)]),
        ("classic", [("doublet", "css/doublet-tokens.css"), ("doublet", None)]),
    ]
    switches = bad = 0
    with Site(tree) as site, Session() as s:
        for theme in a.themes:
            why = core.theme_skip(tree, theme)
            if why:
                rep.skip(why)
                continue
            for start, steps in runs:
                seed = f"try {{ localStorage.setItem('histv-ui-style', '{start}') }} catch (e) {{}}"
                ld = open_page(s, site, tree, theme, start, seed_js=seed)
                try:
                    check_applied(ld.page, theme, start, tree)
                    ld.page.evaluate(SAMPLER_JS)
                    raf2(ld.page)
                    current = start
                    for new, fail in steps:
                        tag = f"{theme}: {current} -> {new}" + (f" with {fail} failing" if fail else "")
                        ok, msg = one_switch(ld.page, site, current, new, a.css_delay, [fail] if fail else [])
                        switches += 1
                        print(f"{'ok  ' if ok else 'FAIL'}  {tag}: {msg}", flush=True)
                        if not ok:
                            bad += 1
                            rep.failures.append((tag, msg))
                            break
                        current = current if fail else new
                    ld.check()
                except CellFailure as e:
                    rep.fail(f"{theme}: from {start}", str(e))
                    bad += 1
                finally:
                    ld.close()
    print(f"\nswitch: {switches} switches checked with the stylesheets {a.css_delay} ms slow, {bad} failed")
    if switches == 0:
        print("nothing was switched, so switch cannot pass")
        return EXIT_NOTHING
    return EXIT_DIFF if bad else EXIT_OK
