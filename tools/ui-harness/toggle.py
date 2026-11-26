"""Toggle integrity: after each live switch, the page equals a fresh load in that style."""
import core
from core import (EXIT_DIFF, EXIT_NOTHING, EXIT_OK, CellFailure, Report, Session, Site, check_applied, open_page, raf2)
from states import STATES, THEMES

HREF = {"doublet": "css/app.css", "classic": "css/app-classic.css"}

SNAP_JS = """() => {
  const html = document.documentElement;
  const cs = getComputedStyle(html);
  const names = new Set();
  const take = (st) => { for (let i = 0; i < st.length; i++) if (st[i].startsWith('--')) names.add(st[i]); };
  const walk = (rules) => { for (const r of rules) { if (r.style) take(r.style); if (r.cssRules) walk(r.cssRules); } };
  take(html.style);
  for (const sh of document.styleSheets) { try { walk(sh.cssRules); } catch (e) {} }
  const custom = {};
  for (const n of [...names].sort()) custom[n] = cs.getPropertyValue(n).trim();
  const inline = {};
  for (let i = 0; i < html.style.length; i++) inline[html.style[i]] = html.style.getPropertyValue(html.style[i]);
  const attrs = {};
  for (const a of html.attributes) attrs[a.name] = a.value;
  const computed = {};
  for (const e of document.querySelectorAll('[id]')) {
    if (computed[e.id]) continue;
    const c = getComputedStyle(e), o = {};
    for (let i = 0; i < c.length; i++) o[c[i]] = c.getPropertyValue(c[i]);
    computed[e.id] = o;
  }
  const link = document.getElementById('ui-style-css');
  return {
    custom, inline, attrs, computed,
    sheets: [...document.styleSheets].map(s => (s.href ? s.href.split('/').slice(-2).join('/') : 'inline') + (s.disabled ? ' (disabled)' : '')),
    linkHref: link ? link.getAttribute('href') : null,
  };
}"""


def settle_switch(page, style):
    """Wait for the marker, the stylesheet link and its loaded sheet, then two frames."""
    page.wait_for_function(
        "(want) => { const l = document.getElementById('ui-style-css');"
        " const h = want === 'classic' ? 'css/app-classic.css' : 'css/app.css';"
        " return document.documentElement.dataset.uiStyle === want && !!l && !!l.sheet && l.getAttribute('href') === h; }",
        arg=style, timeout=5000)
    raf2(page, 150)


def blur(page):
    page.evaluate("document.activeElement && document.activeElement.blur()")


def snapshot(page):
    blur(page)
    return page.evaluate(SNAP_JS)


def diff(switched, fresh, other, style, other_style):
    """-> list of readable difference lines."""
    out = []
    if switched["sheets"] != fresh["sheets"]:
        out.append(f"stylesheets: switched {switched['sheets']}  fresh {fresh['sheets']}")
    # The style attribute is compared property by property below, not as a string.
    sa = {k: v for k, v in switched["attrs"].items() if k != "style"}
    fa = {k: v for k, v in fresh["attrs"].items() if k != "style"}
    if sa != fa:
        out.append(f"<html> attributes: switched {sa}  fresh {fa}")
    for n in sorted(set(switched["inline"]) | set(fresh["inline"])):
        a, b = switched["inline"].get(n), fresh["inline"].get(n)
        if a != b:
            theirs = other["custom"].get(n, "") if n.startswith("--") else ""
            tag = f" (set in a fresh {other_style} load: belongs to {other_style})" if a and not b and theirs else ""
            out.append(f"<html> inline {n}: switched {a!r}  fresh {b!r}{tag}")
    for n in sorted(set(switched["custom"]) | set(fresh["custom"])):
        a, b = switched["custom"].get(n, ""), fresh["custom"].get(n, "")
        if a != b and n not in switched["inline"] and n not in fresh["inline"]:
            theirs = other["custom"].get(n, "")
            tag = f" (belongs to {other_style})" if a and not b and theirs else ""
            out.append(f"<html> custom property {n}: switched {a!r}  fresh {b!r}{tag}")
    left = [ln.split()[2].rstrip(":") for ln in out if f"belongs to {other_style}" in ln]
    if left:
        out.insert(0, f"{len(left)} custom properties of {other_style} remain on <html>: " + ", ".join(left[:8])
                   + (" ..." if len(left) > 8 else ""))
    for i in sorted(set(switched["computed"]) | set(fresh["computed"])):
        if i not in switched["computed"] or i not in fresh["computed"]:
            out.append(f"#{i}: present only in {'fresh' if i in fresh['computed'] else 'switched'} page")
            continue
        for p in sorted(set(switched["computed"][i]) | set(fresh["computed"][i])):
            a, b = switched["computed"][i].get(p), fresh["computed"][i].get(p)
            if a != b:
                out.append(f"#{i} {p}: switched {a!r}  fresh {b!r}")
    return out


def cmd_toggle(a):
    tree = core.prepare_tree(a.ref)
    rep = Report()
    if not tree.has_control:
        rep.skip(f"{tree.spec} has no #sel-ui-style: nothing to toggle")
        print("\ntoggle: nothing was switched, so it cannot pass")
        return EXIT_NOTHING
    themes = a.themes or THEMES
    states = a.states or ["empty", "rows"]
    switches = checks = bad = 0
    cache = {}

    def fresh(s, site, theme, style, state):
        key = (theme, style, state)
        if key not in cache:
            viewport, setup = STATES[state]
            ld = open_page(s, site, tree, theme, style, viewport)
            try:
                check_applied(ld.page, theme, style, tree)
                setup(ld)
                cache[key] = snapshot(ld.page)
                ld.check()
            finally:
                ld.close()
        return cache[key]

    with Site(tree) as site, Session() as s:
        for theme in themes:
            why = core.theme_skip(tree, theme)
            if why:
                rep.skip(why)
                continue
            for state in states:
                where = f"{theme} / {state}"
                viewport, setup = STATES[state]
                ld = open_page(s, site, tree, theme, "doublet", viewport)
                try:
                    check_applied(ld.page, theme, "doublet", tree)
                    setup(ld)
                    current = "doublet"
                    for n in range(1, a.cycles * 2 + 1):
                        style = "classic" if current == "doublet" else "doublet"
                        ld.page.select_option("#sel-ui-style", style)
                        try:
                            settle_switch(ld.page, style)
                        except Exception:
                            got = ld.page.evaluate("({m: document.documentElement.dataset.uiStyle || null, "
                                                   "h: (document.getElementById('ui-style-css')||{getAttribute(){return null}}).getAttribute('href')})")
                            rep.fail(f"{where} switch {n} to {style}",
                                     f"the switch did not take effect (data-ui-style {got['m']}, #ui-style-css href {got['h']}; "
                                     f"expected {style}, {HREF[style]})")
                            bad += 1
                            switches += 1
                            current = style
                            continue
                        ld.check()
                        snap = snapshot(ld.page)
                        lines = diff(snap, fresh(s, site, theme, style, state), fresh(s, site, theme, current, state),
                                     style, current)
                        switches += 1
                        checks += len(snap["computed"])
                        tag = f"{where}  switch {n}: {current} -> {style}"
                        if lines:
                            bad += 1
                            print(f"FAIL  {tag}: {len(lines)} differences from a fresh {style} load")
                            for ln in lines[:a.max_lines]:
                                print(f"        {ln}")
                            if len(lines) > a.max_lines:
                                print(f"        ... {len(lines) - a.max_lines} more")
                            rep.failures.append((tag, f"{len(lines)} differences"))
                        else:
                            print(f"ok    {tag}")
                        current = style
                except CellFailure as e:
                    rep.fail(where, str(e))
                    bad += 1
                finally:
                    ld.close()
    print(f"\ntoggle: {switches} switches checked ({checks} element comparisons), {bad} failed, "
          f"{len(rep.skips)} skip notes")
    if switches == 0:
        print("nothing was switched, so toggle cannot pass")
        return EXIT_NOTHING
    return EXIT_DIFF if bad else EXIT_OK
