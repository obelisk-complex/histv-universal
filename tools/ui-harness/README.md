# UI harness

Headless render, parity, toggle and first-paint checks for the HISTV front end (`src/`, vanilla JS), run in Chromium through Playwright against a mocked Tauri bridge. No Rust backend, no real window, no `DISPLAY`.

It exists to verify the interface-style toggle (Doublet / Classic) before release 3.0; the contract it checks is in `docs/superpowers/specs/2026-11-26-ui-style-toggle-design.md` on the toggle branch.

## Bridge surface the front end uses

Taken from `src/js/app.js` at the harness base (5e73dad) and `e4d0ee2`. `e4d0ee2` calls the same commands except `attach_log_listener`, which only the newer tree has. Line numbers refer to `src/js/app.js` at the base.

### Start-up (every page load, in this order)

| Call | Line | Returns | Used for |
|---|---|---|---|
| `get_config` | 186 | `AppConfig` as camelCase JSON (`theme`, `outputFolder`, `targetBitrate`, ..., `logDrawerOpen`; the toggle adds `uiStyle`) | restores every control; failure is caught and logged, so the page renders with defaults |
| `get_themes` | 278 | `[{name, colors:{background, surface, text, primary, success, error, ...}}]` | fills `#sel-theme`; at the base the four Doublet pairings are prepended in JS |
| `listen(...)` x 20, plus 3 drag-and-drop listeners at 1271-1284 | 2818 | `Promise<unlisten>` | one registration per event in the next table; a rejected registration is only logged |
| `attach_log_listener` | 133 | nothing | replays buffered log lines as `log` events (absent in `e4d0ee2`) |
| `get_encoder_detection_status` | 139 | `bool` | `true` skips the ffmpeg-missing branch |
| `get_detected_encoders` | 142 | `[[{name, codecFamily, isHardware}], [audioEncoderName]]` | encoder summary line, enables Start |
| `get_ffmpeg_missing_status` | 153 | `bool` | only read when detection is not done; `true` opens `#modal-ffmpeg-missing` |

### Events (`listen`)

| Event | Payload |
|---|---|
| `encoder-detection-done` | none |
| `ffmpeg-missing` | none |
| `ffmpeg-download-progress` | string |
| `log`, `ffmpeg-stderr` | string (one log line) |
| `file-progress` | `{percent, timeSecs, totalSecs, pass?:{current,total}}` |
| `queue-item-updated` | `[index, "Encoding"\|"Done"\|...]` |
| `queue-item-probed` | `[index, QueueItem]` |
| `batch-started`, `batch-finished`, `queue-sync-complete` | none |
| `batch-progress` | `{current, total}` |
| `batch-status`, `batch-command`, `wave-status` | string |
| `wave-progress` | `{wave, totalWaves, fileInWave, waveSize}` |
| `overwrite-prompt`, `fallback-prompt` | file name string |
| `toast` | string |
| `post-batch` | `{action, countdown}` |
| `tauri://drag-enter`, `tauri://drag-leave`, `tauri://drag-drop` | drop event (`payload.paths`) |

`QueueItem` is `{fullPath, fileName, baseName, status, sourceBytes}` plus the probe fields `videoCodec, videoWidth, videoHeight, videoBitrateBps, videoBitrateMbps, isHdr, colorTransfer, audioStreams[{index,codec,bitrateKbps}], durationSecs, doviProfile, doviBlCompatId, hasHdr10plus, hdr10plusProfile, videoRotation, videoFps, subtitleStreamCount`. `status` is PascalCase: `Pending, Probing, Encoding, Done, Failed, Skipped, Cancelled`.

### Commands used by the scenarios and by user actions

| Call | Line(s) | Returns |
|---|---|---|
| `plugin:dialog\|open` | 16, 1235, 2355 | selected path(s) or `null` |
| `add_files_to_queue` | 1181 | `{startIndex, count}`; the front end then calls `get_queue` |
| `get_queue` | 1166 | `[QueueItem]` |
| `probe_file` | 1205 | nothing; the backend answers with a `queue-item-probed` event |
| `remove_queue_items`, `requeue_items` | 1352, 1379, 2226, 2250 | nothing (`{indices}`) |
| `requeue_all`, `clear_completed`, `clear_all_queue` | 2238, 2263, 2276 | nothing |
| `move_queue_item` | 1551 | nothing (`{from, to}`) |
| `open_file`, `reveal_file` | 889, 2292, 2305 | nothing (`{path}`) |
| `repair_tags`, `deep_repair_tags` | 2317, 2332 | repaired-row count (`number`) |
| `preflight_check` | 2419 | `[{fileName, sourceType, bestPossible, actualOutcome, missingTool\|null}]`; non-empty opens `#modal-preflight` |
| `download_mp4box`, `download_ffmpeg` | 2426, 165, 2848 | nothing |
| `start_batch` | 2475 | nothing (`{settings}`) |
| `toggle_pause` | 2511 | `bool`, paused |
| `cancel_current`, `cancel_all` | 2348, 2349 | nothing |
| `respond_overwrite`, `respond_fallback` | 2568-2640 | nothing (`{response}`) |
| `execute_post_batch_action` | 2774 | nothing (`{action}`) |
| `save_config` | 234 | nothing (`{config}`), debounced 600 ms after any setting changes |
| `plugin:notification\|is_permission_granted`, `request_permission`, `notify` | 3119-3123 | `bool`, `string`, nothing |

Everything above is answered by `mock-tauri.js`. Any other `invoke` or `listen` fails the run (see Mock).

## Running it

```
tools/ui-harness/run.sh matrix <ref>      # render the state matrix of one ref
tools/ui-harness/run.sh parity            # classic on HEAD against e4d0ee2
tools/ui-harness/run.sh toggle [ref]      # live style switches against fresh loads
tools/ui-harness/run.sh paint [ref]       # persisted style in the first painted frame
tools/ui-harness/run.sh selftest          # the negative controls below (about 30 s)
```

Needs Python 3 with Playwright (`pip install playwright`; tested with 1.62) and a system Chromium (`/usr/bin/chromium`, override with `UI_HARNESS_CHROMIUM`). The harness drives its own headless browser: `run.sh` unsets `DISPLAY` and `WAYLAND_DISPLAY`, and the browser's `HOME` and XDG directories are under the output folder, so nothing is written under `~/.config` or `~/.local`.

Output goes to `$UI_HARNESS_OUT` (default `${TMPDIR:-/tmp}/histv-ui-harness`), never into the repo.

A `ref` is a git ref (`git archive <ref> src src-tauri/themes` into a scratch folder), `@worktree` (the working tree, uncommitted changes included; the default for `toggle` and `paint`) or `dir:PATH` (a folder holding `src/` and `src-tauri/themes/`).

Exit codes, the same for every command: `0` passed, `1` a difference or failed check, `2` nothing was compared or checked (a gate that compared nothing must not pass), `3` harness error (the mock was asked an unknown command or event, or the page threw).

### Style and theme handling

A ref without `#sel-ui-style` renders one style natively: `doublet` if it ships `css/doublet-tokens.css`, otherwise `classic` (`e4d0ee2`). The other style's cells are skipped with a printed `SKIP` note. Themes come from the ref's own `src-tauri/themes/*.json`, so `Classic Dark`, `Classic Light` and (at `e4d0ee2`) `Creditor` are skipped with a note until the ref has them. After every load the harness checks that `#sel-theme` and, where the control exists, `#sel-ui-style` and `data-ui-style` hold the requested values; a page that fell back to a default is a failed cell, not a render of the cell asked for.

## Mock (`mock-tauri.js`, `fixtures.py`)

Injected before `app.js` as `window.__TAURI__`. Fixtures: the `AppConfig` defaults (plus `uiStyle` when a style is requested), the ref's themes, five encoders, and seven queue rows (Pending, Encoding with progress, Done with Dolby Vision, Failed, Skipped, Cancelled, a long file name). A clock override makes the ETA text repeatable.

A command or event the mock does not know is recorded in `window.__MOCK__.failures`, logged, and rejected. The harness reads `failures` and uncaught page errors after every step and ends the run with exit 3, so an app that swallows the rejection in a `try/catch` (as `get_themes` does) still fails the run. Requests to anything but the local server are blocked and also fail the run.

## States

All reached through the app's own controls or backend events, on a queue filled by clicking "Add files":

| State | How |
|---|---|
| `empty` | nothing added |
| `rows` | files added, a batch started, row 2 mid-encode at 41.7% with ETA |
| `drawer` | `rows` plus "Advanced settings" opened |
| `rail-collapsed` | `rows` plus the splitter toggle; the rail must have shrunk |
| `narrow-800` | `rows` at 800 x 900 (the column-mode breakpoint is 820) |
| `modal-overwrite`, `modal-fallback` | `rows` plus the `overwrite-prompt` / `fallback-prompt` event |
| `modal-preflight` | files added (no batch), `preflight_check` answers two warnings, Start clicked |
| `log` | `rows`, six log lines of each class, log panel opened |

Every other state is 1280 x 800. A state whose post-condition does not hold (modal not visible, rail not shrunk, ...) is a failed cell. Transitions and animations are given zero duration from the first line of the page, so geometry and computed styles are read at their end state.

## matrix

`run.sh matrix <ref> [--styles ...] [--themes ...] [--states ...]` writes, per cell, `matrix/<ref>/<style>/<theme>/<state>.png` and `.json` under the output folder, plus `summary.json`. The JSON holds, for every element with an id, `x y w h` (2 decimals), computed `display` and `visibility`, and the active style sheets. Full matrix: 2 styles x 7 themes x 9 states = 126 cells.

## parity

`run.sh parity [--baseline e4d0ee2] [--candidate HEAD] [--baseline-style native] [--candidate-style classic] [--tol 1] [--states ...]`

Renders the same cells on both refs and compares `x y w h`, `display` and `visibility` of every id, and ids present on one side only. Theme pairs: Default Dark with Classic Dark, Default Light with Classic Light, Nord, Jessica Dark (where the candidate lacks the `Classic` name the plain name is used, with a note).

Tolerance: 1 px per coordinate. It covers sub-pixel rounding and font hinting, which measured 0 px between two renders of the same ref (the first control below). Anything above is printed as `DIFF <state> / <theme>  #id  prop: baseline A  candidate B`.

A difference accepted because the plain-language wording changes the text size goes in `parity-explained.json` as `{"id": "btn-x", "reason": "...", "states": [...], "props": [...]}`. It is then printed as `EXPLAINED` with the reason and does not fail the run. The file ships empty; nothing is accepted by default. A report with every difference is written to `parity/`.

## toggle

`run.sh toggle [ref] [--themes ...] [--states empty rows] [--cycles 3]`

For each theme and state: load in doublet, reach the state, then switch `#sel-ui-style` to classic and back, `--cycles` times. After each switch it waits for `data-ui-style`, `#ui-style-css` (href `css/app.css` or `css/app-classic.css`, sheet loaded), then compares with a fresh load in that style, theme and state:

- computed style, every property, of every element with an id;
- `<html>` attributes, inline properties, and every custom property declared in any style sheet or inline, read as computed on `<html>`;
- the list of style sheets and which are disabled.

Custom properties on `<html>` that a fresh load of the other style has and the fresh load of this style does not are reported as `custom properties of <style> remain on <html>`, with names. A switch that does not take effect (marker or link not updated) is a failure.

## paint

`run.sh paint [ref] [--styles classic doublet] [--ipc-delay 300] [--seed JS]`

Load 1 persists the style (`get_config` answers `uiStyle`; whatever the app stores for next time, such as a `localStorage` cache, is stored in a browser context kept for load 2). Load 2 is the watched one: `get_config` and `get_themes` take 300 ms, and a `requestAnimationFrame` loop samples the style signature (`data-ui-style`, `#ui-style-css` href and sheet, the list of enabled style sheets) in every frame, from before any page script runs. The paint-timing entry marks the first painted frame; from that frame on, every sampled frame must show the persisted style's sheets. A style applied only after `get_config` answers fails because the first frames show doublet.

A missing `data-ui-style` while the right sheets are active is reported as a `WARNING`, not a failure.

## Negative controls

`run.sh selftest` runs each command where it must fail and where it must pass and checks the exit code and a line of the output. `stubs.py` builds the failing trees: a copy of the front end at `5e73dad` (the last commit before the style toggle; the selftest also uses it as the tree with no style control) with a minimal implementation of the contract table (`#sel-ui-style`, `data-ui-style`, `#ui-style-css`, disabled Doublet sheets, `Classic Dark`/`Classic Light` themes, `css/app-classic.css` copied from `e4d0ee2`). Variants: `correct` (style applied in `<head>` from a cache), `leaky` (a live switch leaves the other style's custom properties on `<html>`), `late` (style applied only after `get_config`). They are controls for the harness, not a model of the real implementation.

Result on 2026-11-26 (11 of 11 behaved as expected):

| Command | Control | Expected | Got |
|---|---|---|---|
| matrix | mock told to forget `get_themes` (the app catches that error) | exit 3, "mock does not know invoke get_themes" | exit 3, same text |
| matrix | nothing dropped | exit 0 | exit 0 |
| parity | `e4d0ee2` against itself | exit 0, 0 differences | exit 0, 36 cells, 0 differences |
| parity | `e4d0ee2` against HEAD as it renders now (doublet) | exit 1, differences with both values | exit 1, 4688 differences over 36 cells |
| parity | classic requested on HEAD, which has no style control | exit 2 | exit 2, "nothing was compared" |
| toggle | `correct` stub, 2 cycles | exit 0 | exit 0 |
| toggle | `leaky` stub | exit 1, names the leftover properties | exit 1, 21 `--dx-*` properties on `<html>` after doublet to classic |
| toggle | HEAD, no control | exit 2 | exit 2 |
| paint | `correct` stub | exit 0 | exit 0 |
| paint | `late` stub | exit 1 | exit 1, 19 painted frames in doublet until 324 ms |
| paint | HEAD, no control | exit 2 | exit 2 |

Two controls found harness bugs while they were built and are the reason for the current design: a first toggle run reported 187 colour differences between a switched and a fresh page because transitions started before the zero-duration rule was installed (it is now installed before page scripts run), and a first paint run flagged a missing `data-ui-style` on a page whose style sheets were correct (now a warning).

## First run on the trees as they stand (2026-11-26)

The style control does not exist on HEAD yet.

| Command | Result |
|---|---|
| `matrix e4d0ee2` | 36 cells (classic, native; Default Dark, Default Light, Nord, Jessica Dark x 9 states), 0 failed. Skipped with notes: doublet, Creditor, Classic Dark, Classic Light. |
| `matrix HEAD` | 45 cells (doublet, native; the five themes HEAD has x 9 states), 0 failed. Skipped with notes: classic, Classic Dark, Classic Light. |
| `parity` | exit 2, 0 cells compared: HEAD has no classic style to compare. |
| `toggle HEAD`, `paint HEAD` | exit 2, nothing to switch or persist. |

## What the harness cannot cover

- Real WebKitGTK (Linux) and WebView2 (Windows) engines. Everything here is Chromium 157: layout, font metrics, `color-mix`, `@property`, `backdrop-filter` and stylesheet-swap timing can differ in the real webviews. Spec item 6 stays open until someone looks at both.
- The Rust backend: real events and their timing, real probe results, real config persistence, theme JSON loading by `themes.rs`. The mock serves the theme files of the ref directly.
- The system fonts and display scaling of a user's machine (everything renders at device scale 1 with whatever fonts the host has; compare runs only on one machine).
- Pixel comparison: screenshots are for people. Parity and toggle compare geometry and computed styles, not pixels, so a purely visual regression with the same boxes and styles (an image, a gradient the same computed value hides) is not seen.
- Drag and drop, context menu, keyboard paths and the other modals (delete, unknown codec, ffmpeg missing, countdown).

## UNCERTAIN

- **How the persisted style reaches the first frame.** The contract says the persisted style applies before first paint but not by what channel; the config is behind async IPC, so the app needs an early channel (an inline script reading a `localStorage` cache, or a value injected by Rust). `paint` covers a cache the app writes by itself (load 1 then load 2 in one browser context) and, through `--seed`, any channel that can be written as a script. A channel injected by the Tauri runtime before page scripts (such as an initialization script) cannot be reproduced without knowing its name. Smallest contract change: add a row to the table naming the channel, for example "`localStorage['histv.uiStyle']`, written on every change and on every `get_config`", and pass that to `--seed`.
- **Frame sampling is not pixels.** A `requestAnimationFrame` callback runs immediately before the frame it belongs to is painted, which is why `paint` treats its sample as what the frame shows; the harness does not read pixels to confirm it. If the paint-timing entry is missing the first frame with a `<body>` is used and the run says so.
- **Parity depends on identical wording-sized text.** Where the plain-language copy changes the width of a button or label the box changes; those are the differences `parity-explained.json` is for. Until the toggle exists, how many of them there are is not known.
- **Theme names for the parity pairs.** `Classic Dark` and `Classic Light` are assumed to be the exact names (spec decision 3); a different spelling skips the pair with a note rather than failing.

## Files

`run.sh` and `harness.py` (commands), `core.py` (trees, server, browser, dumps), `states.py` (scenarios), `cells.py` (one cell), `fixtures.py`, `mock-tauri.js`, `parity.py`, `toggle.py`, `paint.py`, `stubs.py` and `selftest.py` (controls), `parity-explained.json`.
