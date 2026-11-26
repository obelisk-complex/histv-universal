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
//
//  A live switch must not show a frame without stylesheets, or with one
//  style's sheets and the other's marker. The new sheets are loaded first
//  in extra links with media="not all", which load but do not apply; once
//  all have loaded, one synchronous step swaps them in, flips the marker
//  and calls back so app.js can re-derive the theme before the next frame.
//  If one fails, the staged links are dropped and the old style stays.
// ═══════════════════════════════════════════════════════════════

const UI_STYLE_STORAGE_KEY = 'histv-ui-style';
const UI_STYLE_SHEET = { doublet: 'css/app.css', classic: 'css/app-classic.css' };
const UI_STYLE_DOUBLET_IDS = ['doublet-tokens-css', 'doublet-components-css'];
let uiStylePending = null;   // the live switch waiting for its stylesheets

function rememberUiStyle(style) {
  try {
    localStorage.setItem(UI_STYLE_STORAGE_KEY, style);
  } catch (_) { /* storage unavailable: the config still applies on load */ }
}

// Mark `style` active and turn the Doublet-only sheets on or off. Only for
// sheets that are already in place: the first paint, or a switch whose
// sheets have all loaded.
function markUiStyle(style) {
  const classic = style === 'classic';
  document.documentElement.dataset.uiStyle = classic ? 'classic' : 'doublet';
  for (const id of UI_STYLE_DOUBLET_IDS) document.getElementById(id).disabled = classic;
  rememberUiStyle(document.documentElement.dataset.uiStyle);
}

// Load `href` beside `after` without applying it. -> { link, done: Promise }.
function stageUiStyleSheet(after, href) {
  const link = document.createElement('link');
  link.rel = 'stylesheet';
  link.media = 'not all';
  const done = new Promise((resolve, reject) => {
    link.addEventListener('load', () => (link.sheet ? resolve() : reject(href)), { once: true });
    link.addEventListener('error', () => reject(href), { once: true });
  });
  link.href = href;
  after.after(link);
  return { link, done };
}

// Make `style` ('doublet' or 'classic'; anything else reads as doublet) the
// active style: <html data-ui-style>, the #ui-style-css href, and the two
// Doublet-only stylesheets, which are disabled under Classic.
// `onDone(failedHref)` runs in the same task as the swap, with null on
// success (app.js then re-derives the theme for the style) or the href of
// the stylesheet that did not load, in which case nothing has changed.
// A newer call cancels a switch still loading; its onDone never runs.
function applyUiStyle(style, onDone) {
  const want = style === 'classic' ? 'classic' : 'doublet';
  if (uiStylePending) uiStylePending.cancel();
  uiStylePending = null;

  const main = document.getElementById('ui-style-css');
  if (main.getAttribute('href') === UI_STYLE_SHEET[want]) {
    markUiStyle(want);
    if (onDone) onDone(null);
    return;
  }

  // The main sheet always changes; the Doublet sheets need loading only if
  // they never have (a session that started in Classic).
  const swaps = [[main, stageUiStyleSheet(main, UI_STYLE_SHEET[want])]];
  if (want === 'doublet') {
    for (const id of UI_STYLE_DOUBLET_IDS) {
      const old = document.getElementById(id);
      if (!old.sheet) swaps.push([old, stageUiStyleSheet(old, old.getAttribute('href'))]);
    }
  }

  let live = true;
  const drop = () => { for (const [, s] of swaps) s.link.remove(); };
  const pending = { cancel() { live = false; drop(); } };
  uiStylePending = pending;

  Promise.all(swaps.map(([, s]) => s.done)).then(() => {
    if (!live) return;
    uiStylePending = null;
    for (const [old, s] of swaps) {
      const id = old.id;
      old.remove();
      s.link.id = id;
      s.link.removeAttribute('media');
    }
    markUiStyle(want);
    if (onDone) onDone(null);
  }, (failedHref) => {
    if (!live) return;
    uiStylePending = null;
    drop();
    if (onDone) onDone(failedHref);
  });
}

(() => {
  let stored = null;
  try {
    stored = localStorage.getItem(UI_STYLE_STORAGE_KEY);
  } catch (_) { /* fall back to Doublet */ }
  const style = stored === 'classic' ? 'classic' : 'doublet';
  document.write(`<link rel="stylesheet" id="ui-style-css" href="${UI_STYLE_SHEET[style]}">`);
  markUiStyle(style);
})();
