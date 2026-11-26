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
