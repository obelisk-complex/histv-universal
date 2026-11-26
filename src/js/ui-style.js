// ═══════════════════════════════════════════════════════════════
//  Interface style (Doublet / Classic), applied before first paint.
//
//  Loaded from <head> after the two Doublet stylesheet links. It writes
//  <link id="ui-style-css"> itself with document.write: a link the parser
//  creates blocks first paint until it has loaded, while changing the href
//  of an existing link from script does not (Chromium painted the old
//  stylesheet first). The config cannot be read this early (invoke is
//  asynchronous), so the last applied style is kept in localStorage; app.js
//  calls applyUiStyle() again with the config value once it has loaded, and
//  whenever the setting changes.
// ═══════════════════════════════════════════════════════════════

const UI_STYLE_STORAGE_KEY = 'histv-ui-style';

// Make `style` ('doublet' or 'classic'; anything else reads as doublet) the
// active style: <html data-ui-style>, the #ui-style-css href, and the two
// Doublet-only stylesheets, which are disabled under Classic. app.js then
// re-derives the theme custom properties for the style (applyTheme).
function applyUiStyle(style) {
  const classic = style === 'classic';
  const root = document.documentElement;
  root.dataset.uiStyle = classic ? 'classic' : 'doublet';

  const link = document.getElementById('ui-style-css');
  const href = classic ? 'css/app-classic.css' : 'css/app.css';
  if (link.getAttribute('href') !== href) link.setAttribute('href', href);
  document.getElementById('doublet-tokens-css').disabled = classic;
  document.getElementById('doublet-components-css').disabled = classic;

  try {
    localStorage.setItem(UI_STYLE_STORAGE_KEY, root.dataset.uiStyle);
  } catch (_) { /* storage unavailable: the config still applies on load */ }
}

(() => {
  let stored = null;
  try {
    stored = localStorage.getItem(UI_STYLE_STORAGE_KEY);
  } catch (_) { /* fall back to Doublet */ }
  const style = stored === 'classic' ? 'classic' : 'doublet';
  const href = style === 'classic' ? 'css/app-classic.css' : 'css/app.css';
  document.write(`<link rel="stylesheet" id="ui-style-css" href="${href}">`);
  applyUiStyle(style);
})();
