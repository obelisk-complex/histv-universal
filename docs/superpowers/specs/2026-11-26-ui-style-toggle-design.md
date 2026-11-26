# Interface style toggle (Doublet / Classic)

Release 3.0 ships the Doublet UI refresh. Settings gets a switch so users who prefer the pre-Doublet look keep it.

## Decisions

1. **Two styles:** `doublet` (current) and `classic` (the look at commit `e4d0ee2`, the last commit before Doublet). Default `doublet` for fresh and upgrading installs.
2. **Scope:** classic restores layout, styling and the glass effects. Wording stays as on `main`: the plain-language copy pass fixes wording and is not a style.
3. **Palettes:** the style switch never changes the user's chosen theme. The old Default Dark and Default Light return as two extra built-in themes, "Classic Dark" and "Classic Light" (values from `e4d0ee2:src-tauri/themes/default-dark.json` and `default-light.json`), selectable under either style.
4. **Live switch:** changing the setting applies at once, without a reload, and persists.
5. **Mechanism:** shared markup. A `<link id="ui-style-css">` swaps the stylesheet; `applyTheme` derives the token set of the active style and removes the other style's inline custom properties; `<html data-ui-style="...">` records the active style. No second copy of `index.html` or `app.js`.

## Contract (fixed, so the app and the test harness can be built separately)

| Item | Value |
|---|---|
| Config key (JSON, camelCase like `videoCodec`) | `uiStyle`: `"doublet"` or `"classic"` |
| Rust field | `ui_style: String` in `AppConfig` |
| Settings control | `<select id="sel-ui-style">`, options `doublet` and `classic`, next to `#sel-theme` |
| Active-style marker | `document.documentElement.dataset.uiStyle` (`data-ui-style`) |
| Stylesheet link | `<link id="ui-style-css">`, href `css/app.css` (doublet) or `css/app-classic.css` (classic) |
| Doublet extra stylesheets | `css/doublet-tokens.css`, `css/doublet-components.css`; disabled while classic is active |

## Facts for the implementer

- Same element ids in both styles. Doublet added `dx-*` classes and two stylesheets (`css/doublet-tokens.css`, `css/doublet-components.css`); classic ignores `dx-*` classes.
- Old CSS: `git show e4d0ee2:src/css/app.css`. Old token derivation (`--glass-*` variables): `git show e4d0ee2:src/js/app.js`, function `applyTheme`.
- Rail and splitter behaviour changed after `e4d0ee2` (`--rail-w` on `#main-area`, collapsed toggle, narrow mode). Classic needs the old behaviour; if the current JS cannot serve both, branch on `data-ui-style` and keep one code path where it can.
- Headings such as `Queue / Your files` are split by Doublet CSS; classic must show only the part the old UI showed.
- `src/js/lib.js` holds pure helpers duplicated inline in `app.js`; `lib.test.js` has a drift test. Keep both in step.
- Config: `src-tauri/src/config.rs` (`theme` at about line 12). Add `ui_style` with the same serde conventions as its neighbours; an unknown or missing value reads as `doublet`.
- Built-in themes: `src-tauri/src/themes.rs` and `src-tauri/themes/`; follow the Creditor precedent (commit `a454be7`) for adding built-ins, including the upgrade pass and its tests.

## Verification (all required before release)

1. **Unit:** pure helpers in `lib.test.js`; Rust config tests (default, round trip, unknown value, config without the field); Rust theme tests for the two new built-ins.
2. **Headless render matrix** (Chromium via Playwright, mocked Tauri bridge, never `DISPLAY=:0`): styles x themes (Default Dark, Default Light, Nord, Jessica Dark, Creditor, Classic Dark, Classic Light) x states (empty queue; rows in each status; settings drawer open; rail collapsed; narrow 800px; overwrite, preflight and fallback modals; log panel).
3. **Classic parity:** render `e4d0ee2` and current classic with the same mock; compare bounding boxes of the shared element ids. Every difference is either explained by the new wording or fixed.
4. **Toggle integrity:** doublet to classic to doublet, several times. After each switch the computed styles of the shared ids equal those of a fresh load in that style, and no custom property from the other style remains on `<html>`.
5. **First paint:** the persisted style applies before the first paint (no flash of the wrong style).
6. **Engines:** the real WebKitGTK and WebView2 renders are not available headlessly; list what was and was not covered.
