# HISTV Desktop UI Redesign — Design Specification

Date: 2026-05-02
Status: Draft for review
Scope: Frontend (`src/index.html`, `src/css/app.css`, `src/js/app.js`) only.
Out of scope: Rust core, queue engine, encode pipeline, iOS port.

---

## 1. Goal

Replace the current single-density desktop layout with a responsive, three-breakpoint UI that fits comfortably on a 1366x768 laptop and degrades gracefully to 380x600. The redesign keeps every existing feature, every keyboard shortcut, and the entire Rust contract surface, but reorganises the screen so the queue dominates and settings recede until invoked. No framework introduced; no build pipeline changes.

## 2. Ethos (locked)

- **Drop.** Get files in. The biggest, most obvious affordance at every size is the queue itself; it is also the drop target.
- **Trust.** Defaults are visible but quiet. The current plan is summarised in one line; settings are reachable, never shouting.
- **Glance.** At any moment the user sees what is running, how long it will take, and what broke. Status legible from across the room.

## 3. Architecture

### Stack
Tauri v2, single window. Frontend stays vanilla HTML/JS/CSS. No framework. Rationale: the existing `app.js` (~2700 lines) is procedural with `queueData` as the single source of truth and ~16 named Rust events feeding it; introducing React or Svelte would force a parallel rewrite of the event-listener wiring and bring a build pipeline (currently `frontendDist: ../src` serves raw files). The cost is large and offers no user-visible benefit beyond what plain CSS Grid + `matchMedia` already gives us.

### State

| State | Lives | Persistence |
|------|-------|-------------|
| `queueData`, `selectedRows`, `batchRunning`, `currentEncodingIndex` | `app.js` module scope (unchanged) | None (rebuilt from backend on load) |
| `breakpoint` (`compact` / `medium` / `expanded`) | `app.js` module scope, derived from `matchMedia` listeners | None (recomputed) |
| `sheetOpen` (bool), `sheetSection` (string id) | `app.js` module scope | Persisted to `AppConfig` (see §3 config addition) |
| Encoder/quality/output settings | Rust `AppConfig` via `get_config` / `save_config` | `config.json` (unchanged schema except below) |

### Config addition (minimal)
Two optional fields added to `AppConfig`, both with serde defaults so existing config files load unchanged:

- `uiSheetOpen: bool` (default `false`)
- `uiSheetSection: String` (default `"encoder"`)

No other schema changes. If the user rejects even these two, they can degrade to "always closed on launch" and the spec still works; they are quality-of-life only.

### Build pipeline
Unchanged. `tauri.conf.json` `frontendDist: ../src`. CSS authored as a single `app.css`. JS authored as a single `app.js`. `lib.js` (CommonJS module for tests) untouched.

## 4. Breakpoints

Three breakpoints. Material 3 / Apple HIG canonical widths.

| Name | Width | Layout shape | Typical hosts |
|------|-------|--------------|---------------|
| Compact | <600px | Single column. Queue as cards. Action bar pinned bottom (FAB-style "+" plus Start pill). Settings sheet slides from bottom, full-height when open. | 1/3 split-view iPad parity, laptop-portrait, ultra-narrow window resize. Tauri window minimum is 380x600 so the entire compact range is reachable. |
| Medium | 600-839px | Single column. Queue as a condensed table (5 columns). Action bar pinned top-right. Settings sheet slides from right as a 320px drawer over the queue. | Half-screen on a 1080p monitor, small laptop windowed. |
| Expanded | =840px | Single column. Queue as a full table (9 columns, current set). Action bar pinned top-right. Settings sheet slides from right at 420px (see OQ-1), pushing nothing; queue stays full-width beneath. | 1366x768 laptop, full-screen on any modern monitor. |

The new Tauri minimum window size becomes 380x600 (down from 910x780). See OQ-8.

CSS implements breakpoints via three `@media` queries plus a single JS `matchMedia` listener that sets `data-breakpoint="compact|medium|expanded"` on `<body>` for non-CSS branches (keyboard handlers, table-vs-card rendering switch).

## 5. Components

### 5.1 Queue (canvas)
**Purpose.** Show every queued file and its plan. Always visible. Always full content-width. Always the drop target.

**Anatomy at expanded.** HTML table with columns: select checkbox, filename, from-size, to-size (est.), resolution, HDR, from-bitrate, to-bitrate, status. Column set fixed (resizable headers killed).

**Anatomy at medium.** Same table, condensed to: checkbox, filename, plan badge (combines codec + bitrate target), from->to size, status. Other fields surface via row expansion (click chevron) or tooltip.

**Anatomy at compact.** Each row becomes a card (one card per file, full-width, ~96px tall):
- Line 1: filename (truncated middle, `...` ellipsis), select-checkbox top-right.
- Line 2: plan badge ("HEVC, GPU", "Copy", "Skip") + from->to size.
- Line 3: status pill + per-row progress overlay when active.

**Behaviour.** Drag-drop, paste (Ctrl+V), file-picker via Add button all funnel into `add_files_to_queue`. Selection model unchanged (Click / Shift+Click / Ctrl+Click / Ctrl+A). Right-click and Shift+F10 open the per-row context menu identically to today. Drag-to-reorder unchanged (`move_queue_item`).

### 5.2 Queue row (anatomy)
- **Filename.** Single line, middle-ellipsis. Title attribute carries full path.
- **Plan badge.** Computed via existing `computeTargetBitrateLabel`. One pill at medium/compact; in the expanded table it stays in the to-bitrate column.
- **Source bitrate / target bitrate.** From/To columns at expanded; combined in the plan badge at smaller sizes.
- **Status.** Coloured pill (see §8).
- **Progress overlay.** Background gradient fills the row left-to-right while encoding (see OQ-6 for variant). Numeric percentage and ETA inline within the row at the right edge.

### 5.3 Action bar
**Purpose.** Add files. Start / pause / skip-current / cancel-all the batch.

**Expanded / medium.** Pinned top-right within the queue panel header. Order: Add | Start (primary) | Pause | Skip | Cancel. Disabled state mirrors current `app.js` rules.

**Compact.** Pinned bottom of viewport. Default: a circular "+" FAB on the left, a Start pill on the right. Pause / Skip / Cancel collapse into the Start pill (it morphs into "Pause" when running, with a chevron menu for Skip / Cancel). See OQ-5 for the alternative full-width-bottom-bar layout.

### 5.4 Settings sheet
**Default.** Closed.

**Open.** Slides from the right (expanded, medium) or bottom (compact). Triggered by:
- Cog icon in the top-right of the queue header.
- Tap / click on the quiet status bar (jumps to the relevant section).
- Keyboard: `Ctrl+,` (new; standard preferences shortcut).

**Sections.** Vertical scroll, anchored headings:
- **Encoder.** Codec family (HEVC / AV1), GPU vs CPU, encoder selection (mirrors current `videoEncoders` list). HDR toggle. Compatibility-mode toggle. Preserve-AV1 toggle.
- **Quality.** Rate-control mode (VBR / CQP / CRF). Target bitrate. Peak multiplier. QP I / QP P. CRF. Precision-mode toggle.
- **Output.** Output mode (folder / next-to-source). Output folder path. Overwrite toggle. Delete-source toggle. Auto-clear-queue toggle.
- **Audio.** (Currently inferred from existing config; surface whatever options exist today, no new controls.)
- **Performance.** Threads. Low-priority toggle. Force-local toggle.
- **After batch.** Post-action selector. Custom command. Countdown.
- **Appearance.** Theme picker (moved here from main UI). Notification toggle. Save-log toggle.

**Dismiss.** Esc, click the cog again, click outside the sheet (expanded only; compact and medium use a backdrop tap to dismiss).

**Width / coverage.** See OQ-1 and OQ-2.

### 5.5 Quiet status bar
**Purpose.** A single, calm line that summarises the current plan when the sheet is closed. Always present at the bottom of the queue panel.

**Content.** Examples:
- "HEVC, GPU encode (NVENC), save next to source"
- "AV1, CPU encode, output to ~/Encoded, overwrite on"
- During encoding: switches to "Encoding 3 of 12 - 47% - 4m 12s remaining"

**Behaviour.** Click / tap opens the settings sheet at the section that controls the most prominent element (e.g. clicking the "HEVC, GPU encode" cluster opens the Encoder section). See OQ-3.

### 5.6 Pre-flight warning sheet
**Purpose.** Surface overwrite prompts (`overwrite-prompt` event), fallback prompts (`fallback-prompt` event), ffmpeg-missing prompt (`ffmpeg-missing` event), and any other modal decisions.

**Form.** A sheet, not a `<dialog>`. Full-screen at compact, large modal centred at medium / expanded. Buttons large enough for confident clicks (44px minimum hit target).

### 5.7 Encoder probe / startup state
**Purpose.** Communicate "we are detecting your encoders, hang on" without blocking the user from inspecting their queue.

**Form.** Inline strip in the action bar that reads "Detecting encoders...", auto-replaced by "HEVC, NVENC ready" (or equivalent) when `encoder-detection-done` fires. Start button is disabled until then with a tooltip explaining why. See OQ-7 for splash / banner alternatives.

### 5.8 Empty state
**Purpose.** Tell a brand-new user what to do.

**Form.** Centered inside the queue: large drop icon, primary line "Drop video files here", secondary line "or paste paths (Ctrl+V), or click Add". Secondary "Add" button inline for keyboard discoverability. Vanishes the moment the queue has =1 item.

## 6. Interaction

### Keyboard (preserved verbatim)
- Click row: select.
- Shift+Click: range select.
- Ctrl+Click: toggle individual.
- Ctrl+A: select all.
- Delete / Backspace: remove selected from queue.
- Right-click / Shift+F10: open per-row context menu.
- Enter on a selected row: open file (existing `open_file` invoke).
- New: `Ctrl+,` opens settings sheet. `Esc` closes any open sheet.

### Pointer
- Drag-and-drop files onto the queue: handled by the existing `tauri://drag-*` listeners. Visual: the queue panel border highlights during `drag-enter`.
- Paste (Ctrl+V): handled by the existing paste listener.
- Add button: opens the OS file picker via the dialog plugin.

### Touch
Out of scope. This is a desktop redesign; iOS is parked. However, all primary affordances (FAB, Start pill, sheet dismiss buttons, status pills) are sized to a 44px minimum hit target where it costs nothing to do so. No swipe gestures, no long-press menus, no touch-specific layouts.

## 7. Theming

Theme system unchanged. The 6-colour user-derived palette in `THEMES.md` (background, surface, text, primary, success, error) drives every surface in the redesign exactly as today. Theme picker moves from the main UI footer into Settings sheet -> Appearance.

Where each colour is applied in the redesign:

- **background.** App body, action-bar background, sheet backdrop.
- **surface.** Queue cards, sheet panel, status pills' default state, action bar buttons.
- **text.** Filenames, settings labels. Muted variant for secondary metadata (bitrate, resolution).
- **primary.** Start button, selected-row outline, sheet section headers, progress fill, plan badge accent.
- **success.** Done status pill, "Copy" / "Skip-already-small" pills, completed-row tint.
- **error.** Failed status pill, error log lines, failed-row tint.

Derived variants (muted text, surface-bright, surface-dim, row tints, glass) generate the same way they do today. The redesign introduces no new theme tokens.

## 8. Status hierarchy

Visual loudness, in descending order:

| Status | Pill colour | Row treatment | Loudness rank |
|--------|-------------|---------------|---------------|
| Encoding (active) | primary | Animated progress overlay, slight brightening | 1 (loudest) |
| Failed | error | Muted error tint across row | 2 |
| Paused | amber (derived) | Static halftone overlay | 3 |
| Preparing / probing | primary, half-opacity | Subtle shimmer | 4 |
| Done | success | Muted success tint | 5 |
| Copied | success, outline only | No tint | 6 |
| Skipped (already small) | text-muted, outline only | No tint | 7 |
| Queued | text-muted | No tint | 8 (quietest) |

Active and failed states are loudest because they demand attention; queued is quietest because it is the default and the user already knows files are queued. "Skipped (already small)" and "Copied" use outlined pills to communicate "this is fine, no action needed" without competing for attention with the active row.

## 9. Migration

In-place rewrite of three files:

- `src/index.html` — DOM restructured around `<main id="queue-panel">` + `<aside id="settings-sheet">` + `<footer id="quiet-status-bar">` + `<nav id="action-bar">`. Splitter and right-panel-aside removed.
- `src/css/app.css` — full rewrite. Existing colour-token system retained verbatim; layout rules replaced.
- `src/js/app.js` — surgical edits. Add: `matchMedia` breakpoint listener, sheet open/close, card-renderer-vs-table-renderer branch, `Ctrl+,` handler. Keep: every `invoke` and `listen` call, every state variable, every existing function except those that touch removed DOM (e.g. splitter drag handler).

### Rust contract surface (must remain intact)

**Invoke commands consumed by frontend** (do not rename, do not change argument shape):
`get_encoder_detection_status`, `get_detected_encoders`, `get_ffmpeg_missing_status`, `download_ffmpeg`, `get_config`, `save_config`, `get_themes`, `open_file`, `get_queue`, `add_files_to_queue`, `probe_file`, `clear_all_queue`, `remove_queue_items`, `requeue_items`, `requeue_all`, `move_queue_item`, plus `plugin:dialog|open`.

**Events listened on frontend** (must continue to be emitted by Rust):
`tauri://drag-enter`, `tauri://drag-leave`, `tauri://drag-drop`, `ffmpeg-missing`, `ffmpeg-download-progress`, `log`, `ffmpeg-stderr`, `file-progress`, `queue-item-updated`, `queue-item-probed`, `batch-started`, `batch-progress`, `batch-status`, `batch-command`, `encoder-detection-done`, `overwrite-prompt`, `fallback-prompt`, `toast`, `wave-status`, `queue-sync-complete`.

**Required DOM ids the Rust side does not see directly** but the JS event handlers must continue to populate. The redesign retains the following ids (or wraps them in equivalent semantic elements with the same id) so the existing handlers keep working: `queue-table`, `queue-bod[y]`, `queue-empty-state`, `drop-overlay`, `select-all`, `btn-start`, `btn-pause`, `btn-cancel-current`, `btn-cancel-all`, `encoder-summary`, `num-bitrate`, `num-qp-i`, `num-qp-p`, `num-crf`, `chk-hdr`, `chk-precision`, `chk-preserve-av1`, `chk-compat`, `num-threads`, `chk-low-priority`, `txt-output-folder`, `chk-overwrite`, `chk-delete-source`, `chk-save-log`, `chk-toast`, `sel-theme`, `sel-post-action`, `txt-custom-command`, `num-countdown`, `modal-ffmpeg-missing`, `ffmpeg-dl-yes`, `ffmpeg-dl-no`. The implementation plan must verify each id is preserved before declaring done.

## 10. Out of scope

- Touch optimisation beyond the cheap 44px hit-target guideline.
- iOS port (parked separately).
- Frontend framework migration (React, Svelte, Vue, Solid).
- Theme system overhaul.
- Queue-engine, encode-pipeline, or Rust-core changes.
- Network, Sonarr, Radarr, or any new backend feature.
- Tooltip rewrite.
- Drag-to-reorder behaviour changes.
- Logging panel redesign (the existing log behaviour is preserved as-is, surfaced inside the settings sheet under Performance or a dedicated Logs collapsible).

## 11. Resolved decisions (formerly open questions)

All OQs locked to their recommendations on 2026-05-02 under auto mode. Reversal cost is low for any of them; mark a follow-up issue if user testing dictates otherwise.

- **OQ-1 LOCKED.** Settings sheet width at expanded = fixed 420px.
- **OQ-2 LOCKED.** Sheet at compact fully covers the queue. Dismiss via chevron-down in top-left + Esc + backdrop tap.
- **OQ-3 LOCKED.** Quiet status bar is tappable; opens the section governing the most prominent token clicked.
- **OQ-4 LOCKED.** Breakpoint transitions snap; no animated reflow.
- **OQ-5 LOCKED.** Compact action bar = FAB ("+") on left + Start pill on right. Pause / Skip / Cancel collapse into the Start pill.
- **OQ-6 LOCKED.** Per-row progress = row-background gradient overlay only. Numeric % + ETA inline at right edge.
- **OQ-7 LOCKED.** Encoder probe = inline strip in the action bar; Start disabled until probe done with explanatory tooltip.
- **OQ-8 LOCKED.** Tauri minimum window = 380x600. Update `tauri.conf.json` `minWidth` / `minHeight` accordingly.
- **OQ-9 LOCKED.** Theme picker stays at level-2 disclosure under Settings -> Appearance. Revisit only if user feedback flags discoverability.

## 12. Testing

Manual verification checklist for the implementation plan to enforce:

- Resize the window from 1920px down to 380px in 10px increments. Confirm: snap at 600px boundary; snap at 840px boundary; no element overflows or clips at any width; the queue is always visible.
- Settings sheet opens via cog icon, `Ctrl+,`, and quiet-status-bar click; closes via Esc, cog click, and (expanded) outside-click.
- Drag a folder of mixed video files onto the queue at all three breakpoints; confirm `add_files_to_queue` fires and rows appear.
- Paste a path (Ctrl+V) at all three breakpoints; confirm same.
- Start a batch; confirm `batch-progress`, `file-progress`, `queue-item-updated` events update the row overlays and the quiet status bar text.
- Force ffmpeg-missing on first run; confirm the pre-flight sheet appears and download proceeds.
- Cycle every theme; confirm all status states (queued / preparing / encoding / paused / done / failed / skipped / copied) render with correct colours.
- Restart the app; confirm settings persisted (bitrate, codec, output folder, sheet-open-state if accepted).
- Run `node --test src/js/lib.test.js`; confirm green. The redesign must not change `lib.js` and must not break its tests.

## 13. Success criteria

Observable / verifiable:

- The app is fully usable on a 1366x768 laptop window, fullscreen, with no element overflowing the viewport.
- The Tauri window can resize down to 380x600 with no element overflowing.
- Settings sheet opens and closes within 100ms (measured via DevTools performance trace).
- The queue is the largest element on screen at every breakpoint (=60% of viewport height when no sheet open).
- Every keyboard shortcut from the current build still works.
- Every Rust invoke command and event listed in §9 is still wired.
- `lib.test.js` passes.
- A user with no documentation can drop files in, hit Start, and complete a successful encode without opening the settings sheet.
- No new dependencies in `package.json` or `Cargo.toml`.
