# Doublet design system in the HISTV frontend (dark only)

Scope: look only. Layout stays as it is in `src/` today (profile rail + queue + status strip, breakpoints 1040/820/520). The 2026-05-02 layout plan (queue cards, action bar, settings sheet, 600/840 breakpoints, 380 px window minimum) is not implemented in `src/`; its component names map onto today's DOM as noted below. No Rust, event, ID or data-attribute changes. App icon, desktop file and metainfo untouched; no mark, no light tokens, no toy, no `.dx-glow`.

## Files

- `src/css/doublet-tokens.css` (new): Doublet `tokens.css` trimmed to the dark blocks of the four pairings, `electric` on `:root`. Spectrum and spark tokens dropped (toy and mark only).
- `src/css/doublet-components.css` (new): focus ring, `.dx-label`, `.dx-section-label`, `.dx-seam`, `.dx-badge*`, reduced motion.
- `src/css/app.css`: rewritten against `--dx-*`; no colour literals.
- `src/index.html`: two stylesheet links, `dx-*` classes, label copy. IDs unchanged.
- `src/js/app.js`: `applyTheme` and `STATUS_ICONS` only.
- `THEMES.md`: derivation and override tables.

## Token map

| Today | Doublet |
|---|---|
| `--background` | `--dx-cloth` |
| `--surface`, `-dim`, `-bright` | `--dx-surface`, `-dim`, `-bright` |
| `--text`, `--text-muted` | `--dx-text`, `--dx-text-muted` |
| `--primary` (fills) | `--dx-thread`, with `--dx-on-thread` |
| `--accent` (lines, focus) | `--dx-thread-line` |
| `--row-info`, `--row-done`, `--row-failed`, `--row-cancelled` | `--dx-thread-wash`, `--dx-success-wash`, `--dx-error-wash`, `--dx-warn-wash` |
| `--icon-done`, `--icon-failed`, `--icon-cancelled` | `--dx-success-text`, `--dx-error-text`, `--dx-warn-text` |
| `--glass-border` | `--dx-line` (decorative), `--dx-line-strong` (control boundary) |
| other `--glass-*`, `--shadow-*`, blur | removed: flat grounds, tone step plus hairline |
| `--radius` 12px, `--radius-sm` 7px, pill 999px | `--dx-radius` 2px, `--dx-radius-sm` 0 |
| `--s1`..`--s6` | `--dx-space-1`..`--dx-space-6` |
| `--font-ui`, `--font-mono` | `--dx-font-sans`, `--dx-font-mono` (system fallbacks, no font files) |
| root 13px, sizes 9 to 16px | Doublet 1.25 scale on the same 13px root (density): `md` body, `sm` table data and controls, `xs` labels |
| `--log-*` | kept as aliases onto `--dx-*` so theme overrides still apply |

## Themes

Doublet rule 3 and THEMES.md use the same six seeds (`background`=cloth, `surface`, `text`, `primary`=thread, `success`, `error`). Conflict: Doublet derives at build time, rejects seeds that fail AA or the taste bands, and this task is dark only; HISTV derives at runtime from user JSON, including a light theme and pink accents. Choice: user themes keep working.

- `applyTheme` derives the `--dx-*` set from the six seeds at runtime and sets it inline on `:root`. Text tokens are nudged toward white (black on a light ground) until 4.5:1; line tokens until 3:1. No seed is rejected.
- Four frontend-only themes head the dropdown: Doublet, Doublet Dusk, Doublet Ember, Doublet Filigree. They clear the inline values and set `data-dx-pairing`, so the vendored tokens apply exactly.
- A saved theme is honoured. The config default lives in `src-tauri/src/config.rs` and stays `Default Dark`, so a fresh install opens in Default Dark colours until the user picks Doublet. Doublet applies only when the saved name matches no theme, or when themes fail to load. Making it the default is a one-line Rust change, left to the maintainer.
- Overrides still honoured: `surface-dim`, `surface-bright`, `text-muted`, `accent`, `row-info`, `row-done`, `row-failed`, `row-cancelled`, `icon-*`, `log-*`. No longer used: `row-encoding`, `progress-fill`, `progress-bg` (progress is drawn in thread).
- Default Light and other light JSON themes still render through the same derivation.

## Per component

- Seam: one 2px thread line on the top edge of `#app`. The brand underline, section-header dashes and row edge bars go.
- Queue table (compact fallback at 820px stands in for cards): mono uppercase headers on a strong hairline, hairline rows, hover is a tone step, selected is thread wash, encoding row shows progress as a 2px thread line along its bottom edge. No row tints.
- Status pills: `.status-badge` is a square wash plus glyph plus word. Done success, failed error, cancelled warn, the rest neutral. Icons use `currentColor`.
- Toolbar (action bar): outline buttons in mono; Start is the single filled control. Each dialog's confirm button is the one fill in that view.
- Rail and advanced drawer (settings sheet): surface plus hairline, no shadow. Profile cards square; active is thread wash with a thread border. Section headers become `ADVANCED / VIDEO` labels running into a rule. Segmented toggles, checkboxes and log filters mark the active state with a thread line, not a fill. Radios stay round so they stay distinct from checkboxes.
- Progress: 4px square tracks, thread fill. The percentage readout takes thread colour only at display size while encoding.
- Context menu, tooltip, dialogs: `surface-bright` or `surface` with a strong hairline, square, no shadow or blur; menu hover is thread wash; dialog scrim is cloth at 80%.
- Log: `surface-dim`; error, warn and result lines get a glyph prefix and the status text colour.

## Order
1. Vendor the two Doublet files; link them.
2. Retarget `app.css`.
3. Apply rules 1, 2, 3, 5 and the signature details in HTML and JS.
4. Fonts: stacks only.
5. Verify: `node --test src/js/lib.test.js`; headless render at 380, 700, 1366 with a mocked Tauri bridge; contrast check on every token pair in use, for each built-in theme.
