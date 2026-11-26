"""A minimal stand-in for the style toggle, built from a ref's own front end, used only as a control.

It follows the contract table (select#sel-ui-style, data-ui-style, link#ui-style-css, disabled
Doublet sheets) just far enough to give toggle and paint something to pass or fail on before
the real implementation exists, and to show each command can fail:

  correct  applies the cached style in <head> before first paint and cleans up custom properties
  leaky    as correct, but a live switch leaves the other style's custom properties on <html>
  late     applies the style only after get_config answers (no early path)
"""
import shutil

import core

STUB_JS = r"""
(function () {
  var root = document.documentElement;
  var VARIANT = '__VARIANT__';
  var cur = root.dataset.uiStyle || 'doublet';
  var origApply = window.applyTheme;

  function removeDx() {
    Array.prototype.slice.call(root.style).forEach(function (p) {
      if (p.indexOf('--dx-') === 0) root.style.removeProperty(p);
    });
  }
  function tokens(style, live) {
    if (style === 'classic') {
      if (!(VARIANT === 'leaky' && live)) removeDx();
      root.style.setProperty('--glass-bg', 'rgba(20,20,24,0.55)');
    } else {
      root.style.removeProperty('--glass-bg');
    }
  }
  function sheets(style) {
    var link = document.getElementById('ui-style-css');
    var want = style === 'classic' ? 'css/app-classic.css' : 'css/app.css';
    if (link.getAttribute('href') !== want) link.setAttribute('href', want);
    document.querySelectorAll('link[href*="doublet-"]').forEach(function (l) { l.disabled = style === 'classic'; });
    root.dataset.uiStyle = style;
  }
  function set(style, live) {
    cur = style;
    sheets(style);
    if (style === 'doublet') origApply(document.getElementById('sel-theme').value);
    tokens(style, live);
    if (VARIANT !== 'late') { try { localStorage.setItem('histv.uiStyle', style); } catch (e) {} }
    document.getElementById('sel-ui-style').value = style;
  }
  window.applyTheme = function (name) { origApply(name); tokens(cur, false); };
  document.getElementById('sel-ui-style').addEventListener('change', function (e) { set(e.target.value, true); });
  window.__TAURI__.core.invoke('get_config').then(function (cfg) { set(cfg.uiStyle === 'classic' ? 'classic' : 'doublet', false); });
})();
"""

EARLY_HEAD = r"""<script>
(function () {
  var s = 'doublet';
  try { s = localStorage.getItem('histv.uiStyle') === 'classic' ? 'classic' : 'doublet'; } catch (e) {}
  document.documentElement.dataset.uiStyle = s;
  var d = s === 'classic' ? ' disabled' : '';
  document.write('<link rel="stylesheet" href="css/doublet-tokens.css"' + d + '>' +
    '<link rel="stylesheet" href="css/doublet-components.css"' + d + '>' +
    '<link id="ui-style-css" rel="stylesheet" href="css/' + (s === 'classic' ? 'app-classic.css' : 'app.css') + '">');
})();
</script>"""

LINKS = ('  <link rel="stylesheet" href="css/doublet-tokens.css" />\n'
         '  <link rel="stylesheet" href="css/doublet-components.css" />\n'
         '  <link rel="stylesheet" href="css/app.css" />')


def _replace_once(text, old, new):
    if text.count(old) != 1:
        raise core.HarnessError(f"stub builder: expected exactly one '{old[:50]}...' in index.html")
    return text.replace(old, new)


def build_stub(variant, base_ref="HEAD", classic_ref="e4d0ee2"):
    """-> 'dir:PATH' spec of a tree made from base_ref's front end plus the stub."""
    base = core.prepare_tree(base_ref)
    classic = core.prepare_tree(classic_ref)
    dest = core.OUT / "stubs" / f"{variant}-{base.label}"
    shutil.rmtree(dest, ignore_errors=True)
    shutil.copytree(base.root, dest)
    shutil.copy(classic.src / "css" / "app.css", dest / "src" / "css" / "app-classic.css")
    for name, src in (("Classic Dark", "default-dark.json"), ("Classic Light", "default-light.json")):
        t = (classic.root / "src-tauri" / "themes" / src).read_text()
        (dest / "src-tauri" / "themes" / src.replace("default", "classic")).write_text(
            t.replace('"Default Dark"', f'"{name}"').replace('"Default Light"', f'"{name}"'))
    index = (dest / "src" / "index.html").read_text()
    if variant == "correct":
        index = _replace_once(index, LINKS, "  " + EARLY_HEAD)
    else:
        index = _replace_once(index, '<link rel="stylesheet" href="css/app.css" />',
                              '<link id="ui-style-css" rel="stylesheet" href="css/app.css" />')
    index = _replace_once(index, '<select id="sel-theme"></select>',
                          '<select id="sel-ui-style"><option value="doublet">doublet</option>'
                          '<option value="classic">classic</option></select>\n          <select id="sel-theme"></select>')
    index = _replace_once(index, '<script src="js/app.js"></script>',
                          '<script src="js/app.js"></script>\n  <script src="js/stub-style.js"></script>')
    (dest / "src" / "index.html").write_text(index)
    (dest / "src" / "js" / "stub-style.js").write_text(STUB_JS.replace("__VARIANT__", variant))
    return f"dir:{dest}"
