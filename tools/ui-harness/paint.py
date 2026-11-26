"""First paint: the persisted style must already be applied in the first painted frame."""
import core
from core import EXIT_DIFF, EXIT_NOTHING, EXIT_OK, Report, Session, Site, open_page

# Runs before any page script. One sample per animation frame: requestAnimationFrame callbacks run
# immediately before the frame they belong to is painted, so a sample is what that frame shows.
SAMPLER_JS = """
(() => {
  window.__FRAMES__ = []; window.__PAINTS__ = [];
  const sig = () => {
    const h = document.documentElement, l = document.getElementById('ui-style-css');
    return {
      body: !!document.body, marker: h.dataset.uiStyle || null,
      href: l ? l.getAttribute('href') : null, loaded: l ? !!l.sheet : null,
      sheets: [...document.querySelectorAll('link[rel~=stylesheet]')]
        .map(x => (x.getAttribute('href') || '') + (x.disabled ? '#off' : '')).join('|'),
    };
  };
  const loop = (t) => {
    window.__FRAMES__.push(Object.assign({ t }, sig()));
    if (!window.__FRAMES_STOP__ && window.__FRAMES__.length < 3000) requestAnimationFrame(loop);
  };
  requestAnimationFrame(loop);
  try {
    new PerformanceObserver((l) => l.getEntries().forEach((e) => window.__PAINTS__.push({ name: e.name, t: e.startTime })))
      .observe({ type: 'paint', buffered: true });
  } catch (e) {}
})();
"""

# What the user sees is decided by which stylesheets apply; the data-ui-style marker alone is not
# a visual change (a doublet page may legitimately lack it until the app has read its config).
VISUAL = ("href", "loaded", "sheets")


def same(a, b):
    return all(a[k] == b[k] for k in VISUAL)


def describe(f):
    return f"data-ui-style={f['marker']}, #ui-style-css={f['href']} (sheet loaded: {f['loaded']}), sheets={f['sheets']}"


def judge(frames, paints, want):
    """-> (ok, message). `want` is the style that was persisted."""
    settled = frames[-1]
    if settled["marker"] != want:
        return False, f"the settled page has data-ui-style={settled['marker']}, the persisted style is {want}: it was not applied at all"
    fp = next((p for p in paints if p["name"] == "first-paint"), None) or \
        next((p for p in paints if p["name"] == "first-contentful-paint"), None)
    if fp:
        k = max([i for i, f in enumerate(frames) if f["t"] <= fp["t"]] or [0])
        basis = f"first paint at {fp['t']:.0f} ms"
    else:
        k = next((i for i, f in enumerate(frames) if f["body"]), 0)
        basis = "no paint-timing entry (UNCERTAIN: first frame taken as the first with a <body>)"
    bad = [i for i in range(k, len(frames)) if not same(frames[i], settled)]
    if not bad:
        late_marker = sum(1 for f in frames[k:] if f["marker"] != want)
        note = (f"; WARNING: data-ui-style was not {want} in {late_marker} of them (stylesheets were right)"
                if late_marker else "")
        return True, f"{len(frames) - k} frames from the first painted one all show {want} ({basis}){note}"
    last = max(bad)
    wrong = frames[bad[0]]
    msg = (f"{len(bad)} painted frames show the wrong style ({basis}); the first, at {wrong['t']:.0f} ms, shows "
           f"{describe(wrong)}; the page only reaches {want} at {frames[last + 1]['t']:.0f} ms"
           if last + 1 < len(frames) else f"{len(bad)} painted frames show the wrong style ({basis})")
    return False, msg


def cmd_paint(a):
    tree = core.prepare_tree(a.ref)
    rep = Report()
    if not tree.has_control:
        rep.skip(f"{tree.spec} has no #sel-ui-style: there is no persisted style to apply")
        print("\npaint: nothing was checked, so it cannot pass")
        return EXIT_NOTHING
    theme = a.theme
    why = core.theme_skip(tree, theme)
    if why:
        rep.skip(why)
        return EXIT_NOTHING
    delays = {"get_config": a.ipc_delay, "get_themes": a.ipc_delay}
    checked = bad = 0
    with Site(tree) as site, Session() as s:
        for style in a.styles:
            ctx = s.browser.new_context(viewport={"width": core.VIEWPORT[0], "height": core.VIEWPORT[1]})
            try:
                # Load 1: the style is persisted (config answers uiStyle, the app does whatever it
                # does to remember it for next time, e.g. a localStorage cache).
                first = open_page(s, site, tree, theme, style, context=ctx, kill_motion=False, fake_clock=False)
                core.check_applied(first.page, theme, style, tree)
                first.page.close()
                # Load 2: the one that is watched. Slow IPC, so applying the style after
                # get_config answers shows up as frames in the wrong style.
                second = open_page(s, site, tree, theme, style, delays=delays, context=ctx, kill_motion=False,
                                   fake_clock=False, seed_js=SAMPLER_JS + (a.seed or ""), wait=False)
                core.wait_ready(second.page, timeout=15000)
                second.page.wait_for_timeout(300)
                second.page.evaluate("window.__FRAMES_STOP__ = true")
                second.page.wait_for_timeout(100)
                second.check()
                frames = second.page.evaluate("window.__FRAMES__")
                paints = second.page.evaluate("window.__PAINTS__")
            finally:
                ctx.close()
            if len(frames) < 3:
                rep.fail(style, f"only {len(frames)} frames were sampled")
                bad += 1
                continue
            ok, msg = judge(frames, paints, style)
            checked += 1
            if ok:
                print(f"ok    {style} persisted: {msg}")
            else:
                bad += 1
                print(f"FAIL  {style} persisted: {msg}")
                rep.failures.append((style, msg))
    print(f"\npaint: {checked} persisted styles checked with get_config delayed {a.ipc_delay} ms, {bad} failed")
    if checked == 0:
        return EXIT_NOTHING
    return EXIT_DIFF if bad else EXIT_OK
