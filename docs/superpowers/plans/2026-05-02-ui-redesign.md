# HISTV Desktop UI Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rewrite the HISTV desktop UI to a single-canvas + slide-over-sheet layout that snaps across three breakpoints (compact/medium/expanded), without breaking the Rust contract surface or `lib.test.js`.

**Architecture:** In-place rewrite of three frontend files (`src/index.html`, `src/css/app.css`, `src/js/app.js`) plus a Tauri window-minimum bump and two optional `AppConfig` fields. Layout via CSS Grid with three `@media` queries; JS owns a `matchMedia` watcher that sets `body[data-breakpoint]` and switches table-vs-card render paths. Sheet = a right-anchored / bottom-anchored `<aside>`; quiet status bar = persistent footer; action bar = top-right (or bottom FAB at compact). All existing `invoke`/`listen` wiring is preserved verbatim; the splitter and right-panel-aside layout shim are removed.

**Tech Stack:** Tauri v2 (Rust), vanilla HTML/JS/CSS frontend, `node --test` for JS unit tests, `cargo` for Rust build/tests. No new runtime or build dependencies.

---

## Repository facts (anchor; check before assuming)

- Repo root: `/media/owner/Workspace/histv-universal` (a git submodule of `/media/owner/Workspace`).
- Spec: `/media/owner/Workspace/histv-universal/docs/superpowers/specs/2026-05-02-ui-redesign-design.md`.
- Themes reference: `/media/owner/Workspace/histv-universal/THEMES.md`.
- Branch base: current submodule HEAD on `main` (working tree dirty with `M src-tauri/src/encoder.rs` and untracked `docs/ios-port-plan.md` + the spec/plan paths — DO NOT touch these in this work; branch off cleanly with `git stash --include-untracked` if needed, or just create the branch from current HEAD without staging the dirty files. Plan tasks below pin the no-stash path.).
- Cargo package: `name = "histv"`, `version = "2.6.0"` at `/media/owner/Workspace/histv-universal/src-tauri/Cargo.toml`.
- No root `package.json`. (Version bump scope: Cargo.toml + tauri.conf.json only.)
- Window minimums today: `minWidth: 910`, `minHeight: 780` in `src-tauri/tauri.conf.json`.
- AppConfig location: `/media/owner/Workspace/histv-universal/src-tauri/src/config.rs` line 11; `serde(rename_all = "camelCase")` (verified by the existing `test_camel_case_serialization` test).
- Splitter drag handler: `src/js/app.js` lines ~476-535 (touches `#splitter`, `#splitter-toggle`, `#right-panel`, `#main-area`); a second `#right-panel` ref at line ~1832. To delete in Phase 4.
- Recent commits in this submodule (from `git log --oneline -10`): the active prefix scheme is conventional commits with parenthetical scopes — examples already in tree: `feat(...)`, `fix(...)`, `chore(...)`, `docs(...)`, `test(...)`. **This plan uses `feat(ui):`, `fix(ui):`, `chore(ui):`, `chore(tauri):`, `chore(rust):`, `test(ui):`, `docs(ui):` as the scope tokens.**
- Existing `lib.js` exports (must not change): `formatBytes`, `formatDuration`, `formatEta`, `computeTargetBitrateLabel`, `computeEstimatedSize`, `formatEstimatedSize`. New pure helpers added by this plan go into `lib.js` so `lib.test.js` can cover them.
- Theme tokens (CSS variables; spelling locked, do not rename in CSS rewrite): `--background`, `--surface`, `--text`, `--primary`, `--success`, `--error`, plus the derived ones the runtime computes (e.g. `--text-muted`, `--surface-bright`, `--surface-dim`, row-tint variables — the rewrite reads them, never redefines them).

## Rust contract surface (must remain intact across the whole plan)

**Invoke commands** (consumed by `src/js/app.js`; do not rename, do not change argument shape):
`get_encoder_detection_status`, `get_detected_encoders`, `get_ffmpeg_missing_status`, `download_ffmpeg`, `get_config`, `save_config`, `get_themes`, `open_file`, `get_queue`, `add_files_to_queue`, `probe_file`, `clear_all_queue`, `remove_queue_items`, `requeue_items`, `requeue_all`, `move_queue_item`, `respond_overwrite`, `respond_fallback`, plus `plugin:dialog|open`.

**Events listened** (Rust must continue to emit; we do not touch the emitters):
`tauri://drag-enter`, `tauri://drag-leave`, `tauri://drag-drop`, `ffmpeg-missing`, `ffmpeg-download-progress`, `log`, `ffmpeg-stderr`, `file-progress`, `queue-item-updated`, `queue-item-probed`, `batch-started`, `batch-progress`, `batch-status`, `batch-command`, `encoder-detection-done`, `overwrite-prompt`, `fallback-prompt`, `toast`, `wave-status`, `queue-sync-complete`.

**Required DOM ids** (must remain in the new HTML, exactly once each — Phase 2 verification step enforces):
`queue-table`, `queue-body`, `queue-empty-state`, `drop-overlay`, `select-all`, `btn-start`, `btn-pause`, `btn-cancel-current`, `btn-cancel-all`, `encoder-summary`, `num-bitrate`, `num-qp-i`, `num-qp-p`, `num-crf`, `chk-hdr`, `chk-precision`, `chk-preserve-av1`, `chk-compat`, `num-threads`, `chk-low-priority`, `txt-output-folder`, `chk-overwrite`, `chk-delete-source`, `chk-save-log`, `chk-toast`, `sel-theme`, `sel-post-action`, `txt-custom-command`, `num-countdown`, `modal-ffmpeg-missing`, `ffmpeg-dl-yes`, `ffmpeg-dl-no`. (Note: spec writes `queue-bod[y]` — that is shorthand for `queue-body`. We treat it as `queue-body`.)

---

## Phase 0 — Setup & contract snapshot

### Task 0.1 — Read the spec end-to-end

- [ ] Open `/media/owner/Workspace/histv-universal/docs/superpowers/specs/2026-05-02-ui-redesign-design.md`. Read every section. Pay special attention to §4 Breakpoints, §5 Components, §7 Theming, §8 Status hierarchy, §9 Migration / contract surface, §11 Resolved decisions (OQ-1..OQ-9 are all locked), §12 Testing, §13 Success criteria.

### Task 0.2 — Confirm clean starting point

- [ ] From the repo root, run:
  ```
  git -C /media/owner/Workspace/histv-universal status --short
  git -C /media/owner/Workspace/histv-universal rev-parse --abbrev-ref HEAD
  ```
- [ ] Expected: branch = `main`. Working tree may carry pre-existing modifications to `src-tauri/src/encoder.rs` and untracked `docs/ios-port-plan.md` plus the spec & this plan file. **Do not stage, stash, or revert these.** They are not part of this work.

### Task 0.3 — Create the feature branch

- [ ] Run:
  ```
  git -C /media/owner/Workspace/histv-universal checkout -b feat/ui-redesign-2026-05
  ```
- [ ] Expected: `Switched to a new branch 'feat/ui-redesign-2026-05'`.

### Task 0.4 — Snapshot the Rust contract surface to a scratch artefact

- [ ] Create directory if needed:
  ```
  mkdir -p /media/owner/Workspace/histv-universal/docs/superpowers/notes
  ```
- [ ] Write `/media/owner/Workspace/histv-universal/docs/superpowers/notes/2026-05-02-rust-contract-snapshot.md` with the following content (treat as a permanent artefact — do not delete after the redesign):

  ```markdown
  # Rust contract surface snapshot — 2026-05-02

  Captured before the UI redesign so any regressions in `invoke` / `emit`
  wiring are obvious. Regenerate with:

  ```
  grep -RnE 'invoke_handler|tauri::command|\.emit\(' src-tauri/src/ \
    > docs/superpowers/notes/2026-05-02-rust-contract-snapshot.txt
  grep -nE "invoke\(|listen\(" src/js/app.js \
    >> docs/superpowers/notes/2026-05-02-rust-contract-snapshot.txt
  ```

  ## Invoke commands (frontend → Rust)

  - get_encoder_detection_status, get_detected_encoders
  - get_ffmpeg_missing_status, download_ffmpeg
  - get_config, save_config, get_themes
  - open_file
  - get_queue, add_files_to_queue, probe_file
  - clear_all_queue, remove_queue_items, requeue_items, requeue_all, move_queue_item
  - respond_overwrite, respond_fallback
  - plugin:dialog|open

  ## Events (Rust → frontend)

  - tauri://drag-enter, tauri://drag-leave, tauri://drag-drop
  - ffmpeg-missing, ffmpeg-download-progress
  - log, ffmpeg-stderr
  - file-progress, queue-item-updated, queue-item-probed
  - batch-started, batch-progress, batch-status, batch-command
  - encoder-detection-done
  - overwrite-prompt, fallback-prompt
  - toast, wave-status, queue-sync-complete
  ```

- [ ] Also generate the raw companion file (the markdown links to it):
  ```
  cd /media/owner/Workspace/histv-universal && \
    { grep -RnE 'invoke_handler|tauri::command|\.emit\(' src-tauri/src/ ; \
      grep -nE 'invoke\(|listen\(' src/js/app.js ; } \
    > docs/superpowers/notes/2026-05-02-rust-contract-snapshot.txt
  ```
- [ ] Verify both files exist and are non-empty:
  ```
  wc -l docs/superpowers/notes/2026-05-02-rust-contract-snapshot.{md,txt}
  ```
- [ ] Expected: `.md` ≥ 30 lines; `.txt` ≥ 50 lines.

### Task 0.5 — Baseline test green-bar

- [ ] Run:
  ```
  cd /media/owner/Workspace/histv-universal && node --test src/js/lib.test.js
  ```
- [ ] Expected: `# pass <N>` with `# fail 0`. Record N. Every subsequent verification step uses the same N (this plan never modifies `lib.js`'s existing exports; only adds new ones).

### Task 0.6 — Commit the snapshot

- [ ] Stage and commit only the two snapshot files (NOT the dirty encoder.rs, NOT the spec, NOT this plan):
  ```
  git -C /media/owner/Workspace/histv-universal add \
    docs/superpowers/notes/2026-05-02-rust-contract-snapshot.md \
    docs/superpowers/notes/2026-05-02-rust-contract-snapshot.txt
  git -C /media/owner/Workspace/histv-universal commit -m "docs(ui): snapshot Rust contract surface before UI redesign"
  ```

---

## Phase 1 — Tauri window minimum (OQ-8)

### Task 1.1 — Read current tauri.conf.json window block

- [ ] Open `/media/owner/Workspace/histv-universal/src-tauri/tauri.conf.json`. Locate the `windows[0]` entry (look for the keys `width`, `height`, `minWidth`, `minHeight`). Confirm the current values are `width: 1100`, `height: 915`, `minWidth: 910`, `minHeight: 780`. (If they differ, abort and reconcile with the user — the spec assumed those values.)

### Task 1.2 — Edit window minimums

- [ ] In `/media/owner/Workspace/histv-universal/src-tauri/tauri.conf.json`, change:
  - `"minWidth": 910` → `"minWidth": 380`
  - `"minHeight": 780` → `"minHeight": 600`
- [ ] Leave `width` and `height` (initial size) unchanged.
- [ ] Sanity-check JSON parses:
  ```
  cd /media/owner/Workspace/histv-universal && node -e "JSON.parse(require('fs').readFileSync('src-tauri/tauri.conf.json','utf8'))" && echo OK
  ```
- [ ] Expected: prints `OK`.

### Task 1.3 — Confirm Rust still compiles

- [ ] Run:
  ```
  cd /media/owner/Workspace/histv-universal/src-tauri && cargo check
  ```
- [ ] Expected: `Finished` (warnings allowed). If errors appear, they are unrelated to this 4-line edit; surface them and stop.

### Task 1.4 — Commit

- [ ] ```
  git -C /media/owner/Workspace/histv-universal add src-tauri/tauri.conf.json
  git -C /media/owner/Workspace/histv-universal commit -m "chore(tauri): drop window minimums to 380x600 (OQ-8)"
  ```

---

## Phase 2 — HTML skeleton rewrite (`src/index.html`)

This phase fully rewrites `src/index.html`. The output keeps every required DOM id (see top of plan) and reorganises them into the four canonical landmarks: `<main id="queue-panel">`, `<aside id="settings-sheet">`, `<footer id="quiet-status-bar">`, `<nav id="action-bar">`.

### Task 2.1 — Inventory existing ids before rewrite

- [ ] Capture the existing id list to a temp file (used by Task 2.4 to confirm we did not lose any):
  ```
  cd /media/owner/Workspace/histv-universal && \
    grep -oE 'id="[^"]+"' src/index.html | sort -u > /tmp/histv-ui-ids-before.txt && \
    wc -l /tmp/histv-ui-ids-before.txt
  ```
- [ ] Expected: prints a line count (≥ 60). Keep the file around for Task 2.4.

### Task 2.2 — Write the new index.html

- [ ] Replace the entire contents of `/media/owner/Workspace/histv-universal/src/index.html` with the skeleton below. The structure: `<header id="app-header">` (slim, holds the cog), `<main id="queue-panel">` (drop overlay, empty state, queue table+tbody, queue-card container), `<nav id="action-bar">` (Add FAB + Start pill cluster + encoder probe inline strip), `<footer id="quiet-status-bar">`, `<aside id="settings-sheet">` (all settings inputs live in here, grouped under §5.4 sections), and the two retained modal overlays for ffmpeg-missing and pre-flight (the latter already named `modal-preflight` — leave its id; the spec only enumerates `modal-ffmpeg-missing`).

  ```html
  <!DOCTYPE html>
  <html lang="en">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>Honey, I Shrunk The Vids v2.6.0</title>
    <link rel="stylesheet" href="css/app.css" />
  </head>
  <body data-breakpoint="expanded" data-sheet-open="false">
    <div id="app">

      <!-- ═══ Slim header (cog → opens settings sheet) ═══ -->
      <header id="app-header">
        <div class="app-title">Honey, I Shrunk The Vids</div>
        <button id="btn-open-settings" class="icon-btn" aria-label="Open settings (Ctrl+,)" title="Settings (Ctrl+,)">⚙</button>
      </header>

      <!-- ═══ Queue canvas (always visible, always full content-width) ═══ -->
      <main id="queue-panel" aria-label="File queue">
        <div id="drop-overlay">Drop files or folders here</div>

        <div id="queue-empty-state" class="visible">
          <div class="empty-title">No files in queue</div>
          <div class="empty-hint">Drag files here, paste paths, or use the + button</div>
        </div>

        <!-- Expanded + Medium: table render. Hidden at compact via CSS. -->
        <table id="queue-table" aria-label="Queue (table view)">
          <thead>
            <tr>
              <th class="col-check"><input type="checkbox" id="select-all" title="Select / deselect all" /></th>
              <th class="col-filename">Filename</th>
              <th class="col-from-size">From</th>
              <th class="col-to-size">To (est.)</th>
              <th class="col-resolution">Resolution</th>
              <th class="col-hdr">HDR</th>
              <th class="col-from-bitrate">From bitrate</th>
              <th class="col-to-bitrate">To bitrate</th>
              <th class="col-status">Status</th>
            </tr>
          </thead>
          <tbody id="queue-body"></tbody>
        </table>

        <!-- Compact: card render container. Hidden at medium/expanded via CSS. -->
        <div id="queue-cards" aria-label="Queue (card view)"></div>
      </main>

      <!-- ═══ Action bar (top-right at expanded/medium; bottom FAB+pill at compact) ═══ -->
      <nav id="action-bar" aria-label="Batch controls">
        <button id="btn-add" class="fab" aria-label="Add files">+</button>
        <span id="encoder-probe-strip" class="probe-strip">Detecting encoders…</span>
        <span id="encoder-summary" class="encoder-summary" hidden></span>
        <div class="action-cluster">
          <button id="btn-start" class="btn btn-primary" disabled>Start</button>
          <button id="btn-pause" class="btn" disabled>Pause</button>
          <button id="btn-cancel-current" class="btn" disabled>Skip</button>
          <button id="btn-cancel-all" class="btn" disabled>Cancel</button>
        </div>
      </nav>

      <!-- ═══ Quiet status bar (always present; tap → opens sheet at section) ═══ -->
      <footer id="quiet-status-bar" role="status" aria-live="polite" tabindex="0"
              data-section="encoder">
        <span id="quiet-status-text">Loading…</span>
      </footer>

      <!-- ═══ Settings sheet (right-anchored / bottom-anchored; closed by default) ═══ -->
      <aside id="settings-sheet" aria-label="Settings" aria-hidden="true">
        <header class="sheet-header">
          <button id="btn-sheet-back" class="icon-btn" aria-label="Close settings">⌄</button>
          <h2>Settings</h2>
          <button id="btn-sheet-close" class="icon-btn" aria-label="Close settings">×</button>
        </header>
        <div class="sheet-scroll">

          <section class="sheet-section" id="section-encoder" data-section="encoder">
            <h3>Encoder</h3>
            <!-- Codec family + GPU/CPU radios + encoder dropdown live here.
                 Implementation copies the existing controls 1:1 from the legacy DOM;
                 ids must match the originals where they appear in the required-id list. -->
            <div class="form-row">
              <label>Codec family</label>
              <select id="sel-codec-family">
                <option value="HEVC">HEVC</option>
                <option value="AV1">AV1</option>
              </select>
            </div>
            <div class="form-row">
              <label>Encoder</label>
              <select id="sel-encoder"></select>
            </div>
            <div class="checkbox-row">
              <input type="checkbox" id="chk-hdr" /><label for="chk-hdr">Preserve HDR</label>
            </div>
            <div class="checkbox-row">
              <input type="checkbox" id="chk-compat" /><label for="chk-compat">Compatibility mode</label>
            </div>
            <div class="checkbox-row">
              <input type="checkbox" id="chk-preserve-av1" /><label for="chk-preserve-av1">Preserve existing AV1</label>
            </div>
          </section>

          <section class="sheet-section" id="section-quality" data-section="quality">
            <h3>Quality</h3>
            <div class="form-row">
              <label>Rate control</label>
              <select id="sel-rate-control">
                <option value="VBR">VBR</option>
                <option value="QP">CQP</option>
                <option value="CRF">CRF</option>
              </select>
            </div>
            <div class="form-row">
              <label for="num-bitrate">Target bitrate (Mbps)</label>
              <input type="number" id="num-bitrate" min="0.5" max="200" step="0.1" value="4" />
            </div>
            <div class="form-row">
              <label for="num-qp-i">QP I</label>
              <input type="number" id="num-qp-i" min="0" max="51" value="20" />
            </div>
            <div class="form-row">
              <label for="num-qp-p">QP P</label>
              <input type="number" id="num-qp-p" min="0" max="51" value="22" />
            </div>
            <div class="form-row">
              <label for="num-crf">CRF</label>
              <input type="number" id="num-crf" min="0" max="51" value="20" />
            </div>
            <div class="checkbox-row">
              <input type="checkbox" id="chk-precision" /><label for="chk-precision">Precision mode</label>
            </div>
          </section>

          <section class="sheet-section" id="section-output" data-section="output">
            <h3>Output</h3>
            <div class="form-row">
              <label for="txt-output-folder">Output folder</label>
              <input type="text" id="txt-output-folder" placeholder="(next to source)" />
            </div>
            <div class="checkbox-row">
              <input type="checkbox" id="chk-overwrite" /><label for="chk-overwrite">Overwrite existing</label>
            </div>
            <div class="checkbox-row">
              <input type="checkbox" id="chk-delete-source" /><label for="chk-delete-source">Delete source after success</label>
            </div>
          </section>

          <section class="sheet-section" id="section-performance" data-section="performance">
            <h3>Performance</h3>
            <div class="form-row">
              <label for="num-threads">Threads</label>
              <input type="number" id="num-threads" min="0" max="64" value="0" />
            </div>
            <div class="checkbox-row">
              <input type="checkbox" id="chk-low-priority" /><label for="chk-low-priority">Low CPU priority</label>
            </div>
          </section>

          <section class="sheet-section" id="section-after-batch" data-section="after-batch">
            <h3>After batch</h3>
            <div class="form-row">
              <label for="sel-post-action">Post action</label>
              <select id="sel-post-action">
                <option value="None">None</option>
                <option value="Shutdown">Shutdown</option>
                <option value="Sleep">Sleep</option>
                <option value="Log Out">Log Out</option>
                <option value="Custom Command">Custom Command</option>
              </select>
            </div>
            <div class="form-row" id="row-custom-command">
              <label for="txt-custom-command">Command</label>
              <input type="text" id="txt-custom-command" placeholder="e.g. my-script.sh" />
            </div>
            <div class="form-row" id="row-countdown">
              <label for="num-countdown">Countdown (seconds)</label>
              <input type="number" id="num-countdown" min="0" max="3600" value="0" />
            </div>
          </section>

          <section class="sheet-section" id="section-appearance" data-section="appearance">
            <h3>Appearance</h3>
            <div class="form-row">
              <label for="sel-theme">Theme</label>
              <select id="sel-theme"></select>
            </div>
            <div class="checkbox-row">
              <input type="checkbox" id="chk-toast" /><label for="chk-toast">Show toast notifications</label>
            </div>
            <div class="checkbox-row">
              <input type="checkbox" id="chk-save-log" /><label for="chk-save-log">Save log to disk</label>
            </div>
          </section>

        </div>
      </aside>

      <!-- ═══ Sheet backdrop (click → close at compact + medium) ═══ -->
      <div id="sheet-backdrop" hidden></div>

      <!-- ═══ Modal: ffmpeg missing (kept; ids preserved verbatim) ═══ -->
      <div class="modal-overlay" id="modal-ffmpeg-missing" role="dialog" aria-modal="true">
        <div class="modal-box">
          <div class="modal-title">ffmpeg not found</div>
          <div class="modal-body" id="ffmpeg-missing-body">
            HISTV needs ffmpeg to encode. Download it now?
          </div>
          <div class="modal-buttons">
            <button class="btn btn-primary" id="ffmpeg-dl-yes">Download</button>
            <button class="btn" id="ffmpeg-dl-no">Not now</button>
          </div>
        </div>
      </div>

      <!-- ═══ Modal: pre-flight (overwrite / fallback / DV-HDR10+ warnings) ═══ -->
      <div class="modal-overlay" id="modal-preflight" role="dialog" aria-modal="true">
        <div class="modal-box modal-wide">
          <div class="modal-title" id="modal-preflight-title">Pre-flight check</div>
          <div class="modal-body" id="preflight-body"></div>
          <div class="modal-buttons">
            <button class="btn btn-primary" id="pf-download">Download tools</button>
            <button class="btn" id="pf-continue">Encode anyway</button>
            <button class="btn" id="pf-cancel">Cancel</button>
          </div>
        </div>
      </div>

      <!-- ═══ Modal: post-batch countdown (preserved) ═══ -->
      <div class="modal-overlay" id="modal-countdown" role="dialog" aria-modal="true">
        <div class="modal-box">
          <div class="modal-title">Post-batch action</div>
          <div class="modal-body">Performing action in <strong id="countdown-value">0</strong> seconds…</div>
          <div class="modal-buttons">
            <button class="btn" id="btn-cancel-countdown">Cancel</button>
          </div>
        </div>
      </div>

    </div>
    <script src="js/app.js"></script>
  </body>
  </html>
  ```

  Note on legacy ids not in the spec's required-id list (e.g. `app-header`, `btn-add`, `quiet-status-text`, `sheet-backdrop`, `sel-codec-family`, `sel-encoder`, `sel-rate-control`, `row-custom-command`, `row-countdown`, `btn-sheet-back`, `btn-sheet-close`, `btn-open-settings`, `pf-download`, `pf-continue`, `pf-cancel`, `btn-cancel-countdown`, `countdown-value`, `ffmpeg-missing-body`): these are introduced/retained as needed for the new layout and existing handlers. They are not in the spec's hard-required list but app.js references several of them today; preserving them keeps the JS edits surgical.

### Task 2.3 — Verify HTML parses

- [ ] ```
  cd /media/owner/Workspace/histv-universal && node -e "
    const html=require('fs').readFileSync('src/index.html','utf8');
    if(!/<\/html>\s*$/.test(html)){console.error('no closing html');process.exit(1);}
    console.log('len',html.length);
  "
  ```
- [ ] Expected: `len <number>`.

### Task 2.4 — Verify every required DOM id is present exactly once

- [ ] Run this loop. Expected: every line prints `1`. Any line printing `0` or `≥2` is a bug — fix the HTML before continuing.
  ```
  cd /media/owner/Workspace/histv-universal && \
  for id in queue-table queue-body queue-empty-state drop-overlay select-all \
            btn-start btn-pause btn-cancel-current btn-cancel-all encoder-summary \
            num-bitrate num-qp-i num-qp-p num-crf chk-hdr chk-precision \
            chk-preserve-av1 chk-compat num-threads chk-low-priority \
            txt-output-folder chk-overwrite chk-delete-source chk-save-log chk-toast \
            sel-theme sel-post-action txt-custom-command num-countdown \
            modal-ffmpeg-missing ffmpeg-dl-yes ffmpeg-dl-no; do
    n=$(grep -c "id=\"$id\"" src/index.html)
    printf "%-25s %s\n" "$id" "$n"
  done
  ```
- [ ] Expected: every id printed with count `1`.

### Task 2.5 — Confirm new landmark elements are present

- [ ] ```
  cd /media/owner/Workspace/histv-universal && \
  for sel in 'id="queue-panel"' 'id="settings-sheet"' 'id="quiet-status-bar"' 'id="action-bar"'; do
    grep -c "$sel" src/index.html
  done
  ```
- [ ] Expected: four lines, all `1`.

### Task 2.6 — Commit

- [ ] ```
  git -C /media/owner/Workspace/histv-universal add src/index.html
  git -C /media/owner/Workspace/histv-universal commit -m "feat(ui): rewrite index.html around queue-panel + settings-sheet + quiet-status-bar landmarks"
  ```

---

## Phase 3 — CSS rewrite (`src/css/app.css`)

Full file rewrite. The existing 6-token palette (`--background`, `--surface`, `--text`, `--primary`, `--success`, `--error`) and every derived variant (`--text-muted`, `--surface-bright`, `--surface-dim`, the row-tint vars, the glass vars) are preserved verbatim because the JS theme cycle reads them by name — never rename. Use exactly three `@media` queries: `(max-width: 599px)`, `(min-width: 600px) and (max-width: 839px)`, `(min-width: 840px)`. No transitions on `grid-template-*` or `flex-direction` (OQ-4: snap, never animate reflow).

### Task 3.1 — Read THEMES.md to lock token vocabulary

- [ ] Open `/media/owner/Workspace/histv-universal/THEMES.md`. Confirm the 6 user-set tokens are exactly: `background`, `surface`, `text`, `primary`, `success`, `error`. Capture every derived token name the runtime sets (these appear as `--<name>` in `:root` of the current `app.css` lines 1-80). Treat that derived list as immutable — the rewrite reuses every variable by name.

### Task 3.2 — Read the existing token block

- [ ] Open `/media/owner/Workspace/histv-universal/src/css/app.css` lines 1-80. Copy the entire `:root { ... }` block verbatim into the new file's preamble. (The runtime later overwrites these values from theme JSON — keep them as fallbacks.)

### Task 3.3 — Write the new app.css

- [ ] Replace the entire contents of `/media/owner/Workspace/histv-universal/src/css/app.css` with the structure below. The literal CSS rules are spelled out in full — every selector and property is given. Insert the verbatim `:root` block from Task 3.2 where indicated.

  ```css
  /* HISTV — UI Redesign 2026-05-02
     Layout: CSS Grid. Three @media breakpoints (compact/medium/expanded).
     Theme: 6-token palette + derived variants from THEMES.md (do not rename). */

  /* ── (1) Theme tokens ── PASTE THE :root { ... } BLOCK FROM THE OLD app.css HERE ── */
  :root {
    /* Replace this comment with the full :root block copied verbatim from
       the previous app.css lines 1-80 (--background, --surface, --text,
       --primary, --success, --error, plus every derived variable the
       runtime overwrites). Do not rename anything. */
  }

  /* ── (2) Reset + base ── */
  * { box-sizing: border-box; }
  html, body { margin: 0; padding: 0; height: 100%; }
  body {
    font-family: system-ui, -apple-system, Segoe UI, Roboto, sans-serif;
    background: var(--background);
    color: var(--text);
    overflow: hidden; /* page never scrolls; queue scrolls internally */
  }
  *:not(#log-content):not(#log-content *):not(input):not(textarea):not(select) {
    -webkit-user-select: none;
    user-select: none;
  }

  /* ── (3) App grid (one column; quiet-status-bar pinned bottom) ── */
  #app {
    display: grid;
    grid-template-rows: auto 1fr auto auto; /* header / queue / action-bar / status-bar */
    height: 100vh;
    width: 100vw;
  }

  /* ── (4) Header ── */
  #app-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 8px 16px;
    background: var(--background);
    border-bottom: 1px solid var(--surface);
    min-height: 44px;
  }
  #app-header .app-title { font-weight: 600; }
  .icon-btn {
    background: transparent; border: 0; color: var(--text);
    font-size: 18px; padding: 8px; min-width: 44px; min-height: 44px;
    cursor: pointer; border-radius: 6px;
  }
  .icon-btn:hover { background: var(--surface); }

  /* ── (5) Queue panel ── */
  #queue-panel {
    position: relative;
    overflow: auto;
    background: var(--background);
    padding: 8px 16px;
  }
  #drop-overlay {
    position: absolute; inset: 8px; display: none;
    align-items: center; justify-content: center;
    border: 2px dashed var(--primary);
    border-radius: 8px; color: var(--primary); font-size: 18px;
    background: color-mix(in srgb, var(--primary) 10%, transparent);
    pointer-events: none; z-index: 10;
  }
  body[data-drag-active="true"] #drop-overlay { display: flex; }

  #queue-empty-state {
    display: none; flex-direction: column; align-items: center;
    justify-content: center; height: 100%; color: var(--text-muted, var(--text));
    text-align: center;
  }
  #queue-empty-state.visible { display: flex; }
  .empty-title { font-size: 18px; margin-bottom: 6px; }

  #queue-table {
    width: 100%; border-collapse: collapse; display: none;
  }
  #queue-table th, #queue-table td {
    padding: 6px 8px; border-bottom: 1px solid var(--surface);
    text-align: left; font-size: 13px;
  }
  #queue-table th {
    background: var(--surface);
    position: sticky; top: 0; z-index: 1;
  }

  #queue-cards { display: none; }
  .queue-card {
    background: var(--surface); border-radius: 8px; padding: 12px;
    margin-bottom: 8px; min-height: 96px;
    display: grid; grid-template-rows: auto auto auto; gap: 4px;
  }
  .queue-card .filename {
    font-weight: 500;
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
    direction: rtl; text-align: left; /* middle-truncation effect */
  }
  .queue-card .plan-line { color: var(--text-muted, var(--text)); font-size: 12px; }
  .queue-card .status-line { display: flex; gap: 8px; align-items: center; }

  /* ── (6) Status pills (loudness ranks per spec §8) ── */
  .status-pill {
    display: inline-block; padding: 2px 8px; border-radius: 999px;
    font-size: 12px; font-weight: 500;
  }
  .status-pill.encoding { background: var(--primary); color: var(--background); }
  .status-pill.failed   { background: var(--error);   color: var(--background); }
  .status-pill.paused   { background: var(--cancelled, #f59e0b); color: var(--background); }
  .status-pill.preparing { background: color-mix(in srgb, var(--primary) 50%, transparent); color: var(--text); }
  .status-pill.done     { background: var(--success); color: var(--background); }
  .status-pill.copied,
  .status-pill.skipped {
    background: transparent;
    border: 1px solid var(--success);
    color: var(--success);
  }
  .status-pill.skipped { border-color: var(--text-muted, var(--text)); color: var(--text-muted, var(--text)); }
  .status-pill.queued  { background: transparent; color: var(--text-muted, var(--text)); }

  /* Per-row progress overlay (OQ-6: gradient only, % + ETA inline at right) */
  .row-progress {
    background-image: linear-gradient(to right,
      color-mix(in srgb, var(--primary) 22%, transparent) var(--row-pct, 0%),
      transparent var(--row-pct, 0%));
  }

  /* ── (7) Action bar ── */
  #action-bar {
    display: flex; align-items: center; gap: 8px;
    padding: 8px 16px;
    background: var(--background); border-top: 1px solid var(--surface);
  }
  #action-bar .fab {
    min-width: 44px; min-height: 44px; border-radius: 50%;
    background: var(--primary); color: var(--background);
    border: 0; font-size: 22px; cursor: pointer;
  }
  #action-bar .probe-strip {
    color: var(--text-muted, var(--text)); font-size: 12px; padding: 0 8px;
  }
  #action-bar .encoder-summary { color: var(--text); font-size: 12px; padding: 0 8px; }
  #action-bar .action-cluster { display: flex; gap: 6px; margin-left: auto; }
  .btn {
    min-height: 36px; padding: 0 12px;
    background: var(--surface); color: var(--text);
    border: 1px solid var(--surface); border-radius: 6px;
    cursor: pointer;
  }
  .btn[disabled] { opacity: 0.45; cursor: not-allowed; }
  .btn.btn-primary {
    background: var(--primary); color: var(--background); border-color: var(--primary);
  }

  /* ── (8) Quiet status bar ── */
  #quiet-status-bar {
    background: var(--surface);
    color: var(--text-muted, var(--text));
    padding: 6px 16px;
    font-size: 12px;
    cursor: pointer;
    min-height: 32px;
    display: flex; align-items: center;
  }
  #quiet-status-bar:focus-visible { outline: 2px solid var(--primary); outline-offset: -2px; }

  /* ── (9) Settings sheet (closed by default) ── */
  #settings-sheet {
    position: fixed;
    background: var(--surface);
    color: var(--text);
    transform: translateX(100%);
    z-index: 50;
    display: flex; flex-direction: column;
    box-shadow: -4px 0 16px rgba(0,0,0,0.35);
  }
  body[data-sheet-open="true"] #settings-sheet { transform: translateX(0); }
  body[data-sheet-open="true"] #sheet-backdrop {
    display: block;
    position: fixed; inset: 0; z-index: 40;
    background: rgba(0,0,0,0.4);
  }
  #sheet-backdrop { display: none; }
  .sheet-header {
    display: flex; align-items: center; gap: 8px;
    padding: 8px 12px; border-bottom: 1px solid var(--background);
    min-height: 44px;
  }
  .sheet-header h2 { margin: 0; font-size: 14px; flex: 1; }
  .sheet-scroll { overflow: auto; padding: 12px; flex: 1; }
  .sheet-section { margin-bottom: 24px; }
  .sheet-section h3 {
    color: var(--primary); font-size: 13px; text-transform: uppercase;
    letter-spacing: 0.04em; margin: 0 0 8px;
  }
  .form-row { display: grid; grid-template-columns: 1fr 1fr; gap: 8px; align-items: center; margin-bottom: 6px; }
  .form-row label { font-size: 13px; }
  .form-row input, .form-row select {
    background: var(--background); color: var(--text);
    border: 1px solid var(--background); border-radius: 4px; padding: 6px;
  }
  .checkbox-row { display: flex; align-items: center; gap: 8px; min-height: 32px; }
  .form-row.hidden, #row-custom-command.hidden, #row-countdown.hidden { display: none; }

  /* ── (10) Modals (kept lightweight; sheet-style at compact via @media) ── */
  .modal-overlay {
    position: fixed; inset: 0; background: rgba(0,0,0,0.55);
    display: none; align-items: center; justify-content: center; z-index: 100;
  }
  .modal-overlay.visible { display: flex; }
  .modal-box {
    background: var(--surface); color: var(--text);
    padding: 16px; border-radius: 8px; min-width: 320px; max-width: 90vw;
  }
  .modal-box.modal-wide { min-width: 480px; }
  .modal-title { font-weight: 600; margin-bottom: 8px; }
  .modal-buttons { display: flex; gap: 8px; justify-content: flex-end; margin-top: 12px; }

  /* ────────────────────────────────────────────────────────────── */
  /* @media (1): EXPANDED ≥ 840px — full table; sheet 420px right    */
  /* ────────────────────────────────────────────────────────────── */
  @media (min-width: 840px) {
    #queue-table { display: table; }
    #queue-cards { display: none; }
    #settings-sheet { top: 0; right: 0; height: 100vh; width: 420px; }
    /* No backdrop at expanded (OQ-2) */
    body[data-breakpoint="expanded"][data-sheet-open="true"] #sheet-backdrop { display: none; }
    #action-bar { justify-content: flex-end; }
  }

  /* ────────────────────────────────────────────────────────────── */
  /* @media (2): MEDIUM 600-839px — condensed table; sheet 320px     */
  /* ────────────────────────────────────────────────────────────── */
  @media (min-width: 600px) and (max-width: 839px) {
    #queue-table { display: table; }
    #queue-table .col-resolution,
    #queue-table .col-hdr,
    #queue-table .col-from-bitrate,
    #queue-table .col-to-bitrate { display: none; }
    #queue-cards { display: none; }
    #settings-sheet { top: 0; right: 0; height: 100vh; width: 320px; }
  }

  /* ────────────────────────────────────────────────────────────── */
  /* @media (3): COMPACT < 600px — cards; sheet covers full screen   */
  /* ────────────────────────────────────────────────────────────── */
  @media (max-width: 599px) {
    #queue-table { display: none; }
    #queue-cards { display: block; }
    #settings-sheet { top: 0; right: 0; width: 100vw; height: 100vh; }
    /* Action bar bottom-pinned at compact (OQ-5) */
    #app { grid-template-rows: auto 1fr auto auto; }
    #action-bar {
      position: sticky; bottom: 0;
      justify-content: space-between;
    }
    .modal-box { min-width: 0; width: 100vw; height: 100vh; border-radius: 0; }
  }

  /* ── (11) Snap, never animate reflow (OQ-4) ── */
  *,
  *::before,
  *::after {
    transition-property: none !important;
  }
  /* Re-enable transitions only where they don't reflow grid/flex layouts: */
  .icon-btn, .btn, #quiet-status-bar, .status-pill {
    transition-property: background-color, color, opacity !important;
    transition-duration: 120ms;
  }
  ```

### Task 3.4 — Snap test: exactly three `@media` queries

- [ ] ```
  grep -E '^@media' /media/owner/Workspace/histv-universal/src/css/app.css | wc -l
  ```
- [ ] Expected: `3`. If different, fix the CSS before continuing.

### Task 3.5 — Token-preservation grep

- [ ] ```
  cd /media/owner/Workspace/histv-universal && \
  for tok in --background --surface --text --primary --success --error; do
    n=$(grep -c "$tok" src/css/app.css)
    printf "%-14s %s\n" "$tok" "$n"
  done
  ```
- [ ] Expected: every count ≥ 2 (each token referenced in `:root` and at least one rule).

### Task 3.6 — Commit

- [ ] ```
  git -C /media/owner/Workspace/histv-universal add src/css/app.css
  git -C /media/owner/Workspace/histv-universal commit -m "feat(ui): rewrite app.css for grid layout + 3-breakpoint snap"
  ```

---

## Phase 4 — JS migration (`src/js/app.js` + `src/js/lib.js` + `src/js/lib.test.js`)

Surgical edits only. Keep every existing `invoke(…)` and `listen(…)` call. Remove only code that touched dead DOM (splitter drag, splitter toggle, right-panel-aside resize). Add: matchMedia watcher, sheet open/close, `Ctrl+,` / `Esc` handlers, card-vs-table render branch, quiet-status-bar updater, encoder-probe inline strip handler. New pure helpers go into `lib.js` for unit-test coverage.

### Task 4.1 — Pre-flight: confirm baseline tests still green

- [ ] ```
  cd /media/owner/Workspace/histv-universal && node --test src/js/lib.test.js
  ```
- [ ] Expected: matches Phase 0 baseline (same N, fail 0).

### Task 4.2 — TDD: add `resolveBreakpoint(width)` to `lib.js` (failing test first)

- [ ] In `/media/owner/Workspace/histv-universal/src/js/lib.test.js`, append before the closing of the file (preserve the existing `module.exports` at the top of `lib.js` will be expanded in 4.3):

  ```js
  // ── resolveBreakpoint ────────────────────────────────────────
  const { resolveBreakpoint } = require('./lib');

  describe('resolveBreakpoint', () => {
    it('returns compact below 600', () => {
      assert.equal(resolveBreakpoint(380), 'compact');
      assert.equal(resolveBreakpoint(599), 'compact');
    });
    it('returns medium between 600 and 839 inclusive', () => {
      assert.equal(resolveBreakpoint(600), 'medium');
      assert.equal(resolveBreakpoint(839), 'medium');
    });
    it('returns expanded at 840 and above', () => {
      assert.equal(resolveBreakpoint(840), 'expanded');
      assert.equal(resolveBreakpoint(1920), 'expanded');
    });
    it('clamps non-finite input to expanded', () => {
      assert.equal(resolveBreakpoint(NaN), 'expanded');
      assert.equal(resolveBreakpoint(undefined), 'expanded');
    });
  });
  ```
  (If the existing `lib.test.js` does not already wrap suites with `describe`/`it` — open it and confirm — adapt to match its style. The current file uses `describe` + `it` per the indexed snippet, so this fits.)

- [ ] Run-to-fail:
  ```
  cd /media/owner/Workspace/histv-universal && node --test src/js/lib.test.js
  ```
- [ ] Expected: failure with message containing `resolveBreakpoint is not a function` (or similar — JS does not export it yet).

### Task 4.3 — Implement `resolveBreakpoint` in `lib.js`

- [ ] In `/media/owner/Workspace/histv-universal/src/js/lib.js`:
  - Above the `module.exports = { … }` block, add:
    ```js
    function resolveBreakpoint(width) {
      const w = Number(width);
      if (!Number.isFinite(w)) return 'expanded';
      if (w < 600) return 'compact';
      if (w < 840) return 'medium';
      return 'expanded';
    }
    ```
  - Add `resolveBreakpoint` to `module.exports` (alongside the existing names — do not remove any).

- [ ] Run-to-pass:
  ```
  cd /media/owner/Workspace/histv-universal && node --test src/js/lib.test.js
  ```
- [ ] Expected: all green; new tests included.

- [ ] Commit:
  ```
  git -C /media/owner/Workspace/histv-universal add src/js/lib.js src/js/lib.test.js
  git -C /media/owner/Workspace/histv-universal commit -m "test(ui): add resolveBreakpoint helper + unit tests in lib.js"
  ```

### Task 4.4 — TDD: add `formatQuietStatusPlan(settings)` and `formatQuietStatusEncoding(progress)` to `lib.js`

The quiet status bar shows two states (spec §5.5):
- Plan mode: `"HEVC, GPU encode (NVENC), save next to source"` or `"AV1, CPU encode, output to ~/Encoded, overwrite on"`.
- Encoding mode: `"Encoding 3 of 12 - 47% - 4m 12s remaining"`.

- [ ] In `lib.test.js`, append:
  ```js
  // ── formatQuietStatusPlan ────────────────────────────────────
  const { formatQuietStatusPlan, formatQuietStatusEncoding } = require('./lib');

  describe('formatQuietStatusPlan', () => {
    it('renders HEVC GPU NVENC save-next-to-source', () => {
      const s = { codecFamily: 'HEVC', acceleration: 'GPU',
                  encoderLabel: 'NVENC',
                  outputMode: 'next-to-source', overwrite: false };
      assert.equal(formatQuietStatusPlan(s),
        'HEVC, GPU encode (NVENC), save next to source');
    });
    it('renders AV1 CPU folder + overwrite on', () => {
      const s = { codecFamily: 'AV1', acceleration: 'CPU',
                  encoderLabel: '', outputMode: 'folder',
                  outputFolder: '~/Encoded', overwrite: true };
      assert.equal(formatQuietStatusPlan(s),
        'AV1, CPU encode, output to ~/Encoded, overwrite on');
    });
  });

  describe('formatQuietStatusEncoding', () => {
    it('renders the canonical encoding line', () => {
      // 4m 12s = 252 seconds remaining
      const out = formatQuietStatusEncoding({ current: 3, total: 12,
                                              percent: 47, etaSecs: 252 });
      assert.equal(out, 'Encoding 3 of 12 - 47% - 4m 12s remaining');
    });
  });
  ```
- [ ] Run-to-fail:
  ```
  cd /media/owner/Workspace/histv-universal && node --test src/js/lib.test.js
  ```
- [ ] Expected: failure mentioning `formatQuietStatusPlan is not a function`.

### Task 4.5 — Implement the two formatters in `lib.js`

- [ ] In `lib.js`, add:
  ```js
  function formatQuietStatusPlan(s) {
    const parts = [];
    parts.push(s.codecFamily || 'HEVC');
    const accel = s.acceleration || 'CPU';
    const enc = s.encoderLabel ? ` (${s.encoderLabel})` : '';
    parts.push(`${accel} encode${enc}`);
    if (s.outputMode === 'folder' && s.outputFolder) {
      parts.push(`output to ${s.outputFolder}`);
    } else {
      parts.push('save next to source');
    }
    if (s.overwrite) parts.push('overwrite on');
    return parts.join(', ');
  }

  function formatQuietStatusEncoding(p) {
    const eta = formatEta(p.etaSecs); // "4m 12s remaining" — formatEta exists
    return `Encoding ${p.current} of ${p.total} - ${p.percent}% - ${eta}`;
  }
  ```
- [ ] Add both names to `module.exports`.

- [ ] **Verify `formatEta` produces the exact suffix** by reading the function in `lib.js`. If `formatEta(252)` returns `"4m 12s"` (no `" remaining"` suffix), append `" remaining"` in `formatQuietStatusEncoding`. If it already includes `" remaining"`, do not add it twice. Adjust the test assertion in Task 4.4 accordingly so the output is exactly `Encoding 3 of 12 - 47% - 4m 12s remaining`.

- [ ] Run-to-pass:
  ```
  cd /media/owner/Workspace/histv-universal && node --test src/js/lib.test.js
  ```
- [ ] Expected: all green.

- [ ] Commit:
  ```
  git -C /media/owner/Workspace/histv-universal add src/js/lib.js src/js/lib.test.js
  git -C /media/owner/Workspace/histv-universal commit -m "test(ui): add quiet-status formatters in lib.js with unit tests"
  ```

### Task 4.6 — Remove the splitter / right-panel-aside code from `app.js`

- [ ] In `/media/owner/Workspace/histv-universal/src/js/app.js`, locate and delete:
  - The block ~lines 476-535 that wires `#splitter`, `#splitter-toggle`, `#right-panel`, `#main-area` (mousedown drag + click-toggle handlers).
  - The reference at ~line 1832 to `#right-panel`.
  - Any helper variables defined only for the splitter (do a localised review; if nothing else references them, delete).

- [ ] After deletion, grep to confirm no dangling references remain:
  ```
  cd /media/owner/Workspace/histv-universal && \
    grep -nE 'splitter|#right-panel|#main-area' src/js/app.js
  ```
- [ ] Expected: no output (or only output inside string literals / comments, which is acceptable but should be cleaned if trivial).

### Task 4.7 — Add the `matchMedia` breakpoint watcher

- [ ] In `app.js`, near the top of the IIFE / module body (before render functions), add:
  ```js
  const { resolveBreakpoint } = window.HISTVLib || {};
  // Fallback (browser context exposes lib.js via a tiny shim if needed; otherwise
  // duplicate the function locally — but lib.js is the source of truth.)

  function applyBreakpoint() {
    const w = window.innerWidth;
    const bp = (typeof resolveBreakpoint === 'function')
      ? resolveBreakpoint(w)
      : (w < 600 ? 'compact' : w < 840 ? 'medium' : 'expanded');
    document.body.dataset.breakpoint = bp;
    if (typeof onBreakpointChange === 'function') onBreakpointChange(bp);
  }

  window.addEventListener('resize', applyBreakpoint, { passive: true });
  document.addEventListener('DOMContentLoaded', applyBreakpoint);
  ```

  **Note on lib.js exposure to the browser**: `lib.js` currently uses `module.exports` (CommonJS). The Tauri webview does not have CommonJS. If `app.js` cannot import from `lib.js` directly, do one of:
  - (a) Add a guarded `if (typeof window !== 'undefined') window.HISTVLib = { … all exports … };` at the bottom of `lib.js` (Node's `require` ignores `window` because `typeof window === 'undefined'`).
  - (b) Inline `resolveBreakpoint` locally in `app.js` *and* keep it in `lib.js` for tests. Pick (a) — single source of truth.

- [ ] Apply (a): in `lib.js`, after `module.exports = { … }`, add:
  ```js
  if (typeof window !== 'undefined') {
    window.HISTVLib = module.exports;
  }
  ```

- [ ] In `index.html`, ensure `<script src="js/lib.js"></script>` is included before `<script src="js/app.js"></script>`. Verify Task 2.2's HTML includes it; if not, add the line above the `app.js` script tag.

- [ ] Re-run tests:
  ```
  cd /media/owner/Workspace/histv-universal && node --test src/js/lib.test.js
  ```
- [ ] Expected: still green.

- [ ] Commit:
  ```
  git -C /media/owner/Workspace/histv-universal add src/js/app.js src/js/lib.js src/index.html
  git -C /media/owner/Workspace/histv-universal commit -m "feat(ui): add matchMedia breakpoint watcher + remove splitter wiring"
  ```

### Task 4.8 — Sheet open/close + keyboard shortcuts

- [ ] In `app.js`, add (near the breakpoint watcher):
  ```js
  function openSettingsSheet(section) {
    document.body.dataset.sheetOpen = 'true';
    document.querySelector('#settings-sheet').setAttribute('aria-hidden', 'false');
    if (section) {
      const target = document.querySelector(
        `#settings-sheet [data-section="${section}"]`);
      if (target) target.scrollIntoView({ block: 'start' });
    }
    persistSheetState(true, section);
  }
  function closeSettingsSheet() {
    document.body.dataset.sheetOpen = 'false';
    document.querySelector('#settings-sheet').setAttribute('aria-hidden', 'true');
    persistSheetState(false, null);
  }
  function persistSheetState(open, section) {
    // Best-effort: only save if the Rust AppConfig accepts the fields
    // (Phase 5 adds them; before then this is a no-op-or-noisy save).
    try {
      // Existing code path: reuse the debounced saveConfig if it exists.
      if (typeof scheduleConfigSave === 'function') scheduleConfigSave();
    } catch (_) {}
  }

  // Cog button in the slim header.
  document.addEventListener('DOMContentLoaded', () => {
    const cog = document.querySelector('#btn-open-settings');
    if (cog) cog.addEventListener('click', () => openSettingsSheet());
    const back = document.querySelector('#btn-sheet-back');
    if (back) back.addEventListener('click', closeSettingsSheet);
    const close = document.querySelector('#btn-sheet-close');
    if (close) close.addEventListener('click', closeSettingsSheet);
    const backdrop = document.querySelector('#sheet-backdrop');
    if (backdrop) backdrop.addEventListener('click', closeSettingsSheet);
  });

  // Ctrl+, opens settings; Esc closes any open sheet.
  window.addEventListener('keydown', (e) => {
    if ((e.ctrlKey || e.metaKey) && e.key === ',') {
      e.preventDefault();
      openSettingsSheet();
    } else if (e.key === 'Escape') {
      if (document.body.dataset.sheetOpen === 'true') {
        e.preventDefault();
        closeSettingsSheet();
      }
    }
  });
  ```

- [ ] Visual verification (manual; no test): start dev with `cd src-tauri && cargo run`, click the cog → sheet appears; press Esc → closes; press `Ctrl+,` → opens.

- [ ] Commit:
  ```
  git -C /media/owner/Workspace/histv-universal add src/js/app.js
  git -C /media/owner/Workspace/histv-universal commit -m "feat(ui): add settings-sheet open/close + Ctrl+, and Esc shortcuts"
  ```

### Task 4.9 — Card-vs-table render branch

- [ ] In `app.js`, locate the existing function that re-renders the queue (search for `queue-body`, the `<tbody>` population — likely a function called `renderQueue` or similar). Rename the existing function to `renderQueueTableFull(queueData, settings)` (preserves current 9-column layout for expanded mode).

- [ ] Add `renderQueueTableCondensed(queueData, settings)`: same as `renderQueueTableFull` but skips the `<td>` cells for resolution / HDR / from-bitrate / to-bitrate columns (CSS hides the headers; the JS may still emit the cells and let CSS hide them — simpler. If you choose to skip cells, ensure the row's `<tr>` still has the right colspan or simply drop those `<td>` elements). Keep all selection + per-row event wiring identical.

- [ ] Add `renderQueueCards(queueData, settings)`:
  ```js
  function renderQueueCards(queueData, settings) {
    const container = document.querySelector('#queue-cards');
    if (!container) return;
    container.innerHTML = '';
    queueData.forEach((item, i) => {
      const card = document.createElement('div');
      card.className = 'queue-card';
      card.dataset.index = String(i);
      card.innerHTML = `
        <div class="filename" title="${escapeHtml(item.filename || '')}">${escapeHtml(item.filename || '')}</div>
        <div class="plan-line">
          <span class="plan-badge">${escapeHtml(planBadge(item, settings))}</span>
          <span class="size-line">${escapeHtml(formatBytes(item.sourceBytes))} → ${escapeHtml(formatEstimatedSize(item, settings))}</span>
        </div>
        <div class="status-line">
          <span class="status-pill ${item.status || 'queued'}">${escapeHtml(item.status || 'Queued')}</span>
        </div>
      `;
      container.appendChild(card);
    });
  }
  ```
  (Use the existing `escapeHtml` helper if defined; if not, add a one-liner.)

- [ ] Add the dispatcher and wire it as the new top-level render entry-point:
  ```js
  function renderQueue(queueData, settings) {
    const bp = document.body.dataset.breakpoint || 'expanded';
    if (bp === 'compact')      renderQueueCards(queueData, settings);
    else if (bp === 'medium')  renderQueueTableCondensed(queueData, settings);
    else                       renderQueueTableFull(queueData, settings);
    updateEmptyState(queueData);
  }
  function updateEmptyState(queueData) {
    const el = document.querySelector('#queue-empty-state');
    if (el) el.classList.toggle('visible', !queueData || queueData.length === 0);
  }

  // Re-render on breakpoint change (defined earlier as onBreakpointChange).
  function onBreakpointChange(_bp) {
    if (typeof lastQueueData !== 'undefined' && typeof lastSettings !== 'undefined') {
      renderQueue(lastQueueData, lastSettings);
    }
  }
  ```
  Replace every existing call site of the old render function (e.g. `renderQueue(...)` or whatever its name was) with the new `renderQueue(queueData, settings)`. Make sure the closure variables `lastQueueData` / `lastSettings` are kept in scope (assign them whenever new queue data arrives via `get_queue` or `queue-item-updated`).

- [ ] Visual verification: start dev, drop 2-3 files, resize between 380px / 700px / 1100px — confirm cards / condensed-table / full-table all populate.

- [ ] Commit:
  ```
  git -C /media/owner/Workspace/histv-universal add src/js/app.js
  git -C /media/owner/Workspace/histv-universal commit -m "feat(ui): branch queue render between cards / condensed table / full table"
  ```

### Task 4.10 — Quiet status bar updater + click-to-open-sheet

- [ ] In `app.js`, add:
  ```js
  function updateQuietStatusBar() {
    const el = document.querySelector('#quiet-status-text');
    if (!el) return;
    const settings = collectCurrentSettings(); // existing helper that reads inputs
    const planSettings = {
      codecFamily: settings.targetCodecFamily || 'HEVC',
      acceleration: deriveAcceleration(settings),
      encoderLabel: deriveEncoderLabel(settings),
      outputMode: settings.outputMode || 'next-to-source',
      outputFolder: settings.outputFolder || '',
      overwrite: !!settings.overwrite,
    };
    el.textContent = window.HISTVLib.formatQuietStatusPlan(planSettings);
    document.querySelector('#quiet-status-bar').dataset.section = 'encoder';
  }

  function updateQuietStatusBarEncoding(progress) {
    const el = document.querySelector('#quiet-status-text');
    if (!el) return;
    el.textContent = window.HISTVLib.formatQuietStatusEncoding(progress);
  }

  // Click → open the sheet at the section governing the most prominent token (OQ-3).
  document.addEventListener('DOMContentLoaded', () => {
    const bar = document.querySelector('#quiet-status-bar');
    if (bar) bar.addEventListener('click', () => {
      openSettingsSheet(bar.dataset.section || 'encoder');
    });
  });
  ```
  Implement `deriveAcceleration` and `deriveEncoderLabel` as small helpers that look at the encoder selection and return `'GPU' | 'CPU'` and an encoder-name string (e.g. `'NVENC'`). If the existing app.js already exposes those concepts under different names, reuse them.

- [ ] Wire updates:
  - Call `updateQuietStatusBar()` at the end of any settings-change handler that already calls `scheduleConfigSave` / `saveConfig`.
  - In the existing `listen('batch-progress', …)` handler, call `updateQuietStatusBarEncoding({current, total, percent, etaSecs})` with whatever payload the event provides (use the existing field names; if `etaSecs` is absent, derive from elapsed × percent or pass `0` and treat as `'0s remaining'`).
  - When the batch completes (`listen('queue-sync-complete', …)` or equivalent), call `updateQuietStatusBar()` to revert to plan mode.

- [ ] Visual verification: change the codec dropdown and confirm the bar text updates. Start a small batch and confirm the bar switches to encoding mode and back.

- [ ] Commit:
  ```
  git -C /media/owner/Workspace/histv-universal add src/js/app.js
  git -C /media/owner/Workspace/histv-universal commit -m "feat(ui): add quiet status bar updater (plan + encoding modes)"
  ```

### Task 4.11 — Encoder probe inline strip handler

- [ ] In `app.js`, locate the existing `listen('encoder-detection-done', …)` handler. Inside it (or right after it), add:
  ```js
  // Replace the placeholder strip text once the probe finishes.
  const probe = document.querySelector('#encoder-probe-strip');
  const summary = document.querySelector('#encoder-summary');
  if (probe) probe.hidden = true;
  if (summary) {
    summary.hidden = false;
    summary.textContent = buildEncoderSummary(); // existing helper
  }
  // Enable Start button (was disabled until probe done — OQ-7).
  const start = document.querySelector('#btn-start');
  if (start) start.disabled = false;
  ```
- [ ] Confirm the initial HTML state is correct: `#btn-start[disabled]` is set in Task 2.2's HTML; `#encoder-probe-strip` shows "Detecting encoders…" by default; `#encoder-summary[hidden]` until the probe completes.

- [ ] Commit:
  ```
  git -C /media/owner/Workspace/histv-universal add src/js/app.js
  git -C /media/owner/Workspace/histv-universal commit -m "feat(ui): wire encoder probe inline strip + Start enable on probe-done"
  ```

### Task 4.12 — Contract grep: every invoke + listen still present

- [ ] ```
  cd /media/owner/Workspace/histv-universal && \
    grep -cE "invoke\('([a-z_]+)'" src/js/app.js
  ```
- [ ] Expected: count ≥ 17 (the original surface).

- [ ] ```
  cd /media/owner/Workspace/histv-universal && \
    grep -nE "invoke\('" src/js/app.js | \
    sed -E "s/.*invoke\('([a-z_:|\-]+)'.*/\1/" | sort -u
  ```
- [ ] Expected: every command from the snapshot file (`docs/superpowers/notes/2026-05-02-rust-contract-snapshot.md`) appears in this output.

- [ ] ```
  cd /media/owner/Workspace/histv-universal && \
    grep -nE "listen\('" src/js/app.js | \
    sed -E "s/.*listen\('([a-z:/\-]+)'.*/\1/" | sort -u
  ```
- [ ] Expected: every event from the snapshot appears.

- [ ] If anything is missing, restore it before continuing — likely the splitter-removal in Task 4.6 was over-eager.

### Task 4.13 — Final test green

- [ ] ```
  cd /media/owner/Workspace/histv-universal && node --test src/js/lib.test.js
  ```
- [ ] Expected: all green (Phase 0 baseline + 4 new resolveBreakpoint tests + 3 new quiet-status tests).

---

## Phase 5 — Rust optional config fields

### Task 5.1 — Locate AppConfig and confirm conventions

- [ ] Open `/media/owner/Workspace/histv-universal/src-tauri/src/config.rs`. Confirm:
  - The struct at line 11 uses `#[serde(rename_all = "camelCase")]` (verified by `test_camel_case_serialization`).
  - Existing fields use `snake_case` Rust names → `camelCase` JSON keys.
  - `impl Default for AppConfig` exists at ~line 45.
  - There is a `test_partial_json_loads` or similar test (~line 195-200) that loads `{}` and confirms defaults.

### Task 5.2 — Add the two optional fields

- [ ] In the `AppConfig` struct definition (around line 11), add (placement at the bottom of the struct is fine):
  ```rust
  /// Whether the settings sheet was open at last save.
  /// Spec §3 (UI redesign): persisted across launches; default closed.
  #[serde(default)]
  pub ui_sheet_open: bool,

  /// Section the settings sheet should open to.
  /// Spec §3: encoder | quality | output | performance | after-batch | appearance.
  #[serde(default = "default_ui_sheet_section")]
  pub ui_sheet_section: String,
  ```

- [ ] Above `impl Default for AppConfig`, add:
  ```rust
  fn default_ui_sheet_section() -> String { "encoder".to_string() }
  ```

- [ ] In `impl Default for AppConfig`, add the two fields:
  ```rust
  ui_sheet_open: false,
  ui_sheet_section: "encoder".to_string(),
  ```

### Task 5.3 — Add a serde-default round-trip test for the two new fields

- [ ] In `config.rs`, near the other tests, add:
  ```rust
  #[test]
  fn test_ui_sheet_defaults_load_from_partial_json() {
      let config: AppConfig = serde_json::from_str("{}").unwrap();
      assert!(!config.ui_sheet_open);
      assert_eq!(config.ui_sheet_section, "encoder");
  }

  #[test]
  fn test_ui_sheet_round_trip() {
      let config = AppConfig::default();
      let json = serde_json::to_string(&config).unwrap();
      assert!(json.contains("uiSheetOpen"));
      assert!(json.contains("uiSheetSection"));
      let back: AppConfig = serde_json::from_str(&json).unwrap();
      assert_eq!(back.ui_sheet_open, false);
      assert_eq!(back.ui_sheet_section, "encoder");
  }
  ```

### Task 5.4 — Confirm existing Rust tests still pass

- [ ] ```
  cd /media/owner/Workspace/histv-universal/src-tauri && cargo test --lib config::
  ```
- [ ] Expected: all tests in `config.rs` pass, including the two new ones. If `cargo test --lib config::` filters too narrowly, run `cargo test --lib` and grep for `test_ui_sheet`.

### Task 5.5 — Confirm existing config.json files load without error

- [ ] (No filesystem touch — the `#[serde(default)]` annotations guarantee this. The new round-trip test above is the proof.)

### Task 5.6 — Commit

- [ ] ```
  git -C /media/owner/Workspace/histv-universal add src-tauri/src/config.rs
  git -C /media/owner/Workspace/histv-universal commit -m "feat(rust): add optional uiSheetOpen + uiSheetSection to AppConfig"
  ```

---

## Phase 6 — Verification (all gates green before Phase 7)

Each task is an atomic checklist item. Run them in order; do not skip.

### Task 6.1 — JS unit tests green

- [ ] ```
  cd /media/owner/Workspace/histv-universal && node --test src/js/lib.test.js
  ```
- [ ] Expected: `# pass <N+7>` (baseline + 4 resolveBreakpoint + 3 quiet-status), `# fail 0`.

### Task 6.2 — Rust release build green

- [ ] ```
  cd /media/owner/Workspace/histv-universal/src-tauri && cargo build --release
  ```
- [ ] Expected: `Finished release [optimized] target(s)`. Warnings allowed.

### Task 6.3 — Rust tests green

- [ ] ```
  cd /media/owner/Workspace/histv-universal/src-tauri && cargo test --lib
  ```
- [ ] Expected: all green, including the two new `test_ui_sheet_*` tests.

### Task 6.4 — Snap test on @media count

- [ ] ```
  grep -E '^@media' /media/owner/Workspace/histv-universal/src/css/app.css | wc -l
  ```
- [ ] Expected: `3`.

### Task 6.5 — Required-id sweep (re-run from Phase 2)

- [ ] Re-run the loop from Task 2.4. Expected: every id prints `1`.

### Task 6.6 — Contract grep (re-run from Phase 4)

- [ ] Re-run Task 4.12. Expected: every original `invoke` + `listen` still present.

### Task 6.7 — Manual: resize sweep 1920 → 380

- [ ] Launch the app: `cd src-tauri && cargo run`.
- [ ] Drop 3-5 mixed video files into the queue.
- [ ] Slowly resize the window from 1920px wide down to 380px wide. Confirm:
  - At ≥ 840px: full table (9 columns).
  - At 839px: snap to condensed table (5 columns).
  - At 599px: snap to cards.
  - No element clips, overflows, or misaligns at any width.
  - The queue is always visible.

### Task 6.8 — Manual: keyboard shortcut sweep (spec §6)

- [ ] Click a row → selected.
- [ ] Shift+Click another row → range select.
- [ ] Ctrl+Click a row → toggle individual.
- [ ] Ctrl+A → all selected.
- [ ] Delete or Backspace → selected rows removed (calls `remove_queue_items`).
- [ ] Right-click row → context menu opens. Press Shift+F10 on a row → context menu opens.
- [ ] Enter on a selected row → opens the file (`open_file` invoke).
- [ ] Ctrl+, → settings sheet opens.
- [ ] Esc → settings sheet closes.

### Task 6.9 — Manual: drag-drop + paste at all 3 breakpoints

- [ ] At 1100px (expanded), drag a folder onto the queue. Expect: `add_files_to_queue` fires, rows appear, drop overlay highlights during drag-enter and clears on drop.
- [ ] Resize to 700px (medium). Repeat. Expect same.
- [ ] Resize to 420px (compact). Repeat. Expect same.
- [ ] At one breakpoint, paste a file path with Ctrl+V. Expect rows added.

### Task 6.10 — Manual: theme cycle

- [ ] Open settings sheet → Appearance → cycle through every theme. For each theme, view at least one queue row in each of these states (use a small batch with deliberately failing items to surface them):
  - queued, preparing, encoding, paused, done, failed, copied, skipped.
- [ ] Confirm the colours render correctly per spec §8 (encoding loudest; queued quietest).

### Task 6.11 — Manual: persisted settings + sheet state

- [ ] Set a non-default bitrate, codec, output folder. Open the settings sheet. Quit the app. Relaunch.
- [ ] Confirm: all settings persisted; the settings sheet is open at the section it was last on (`ui_sheet_open: true`, `ui_sheet_section: "<section>"`).
- [ ] Inspect the saved config: `cat ~/.config/histv/config.json` (or platform-equivalent). Confirm `uiSheetOpen` and `uiSheetSection` keys are present.

### Task 6.12 — Manual: ffmpeg-missing first-run dry test

- [ ] If a system `ffmpeg` is on PATH, temporarily move it: `sudo mv $(which ffmpeg) /tmp/ffmpeg-backup`.
- [ ] Launch the app: `cd src-tauri && cargo run`.
- [ ] Confirm `#modal-ffmpeg-missing` appears with `Download` and `Not now` buttons.
- [ ] Click `Not now` → modal closes; pre-flight sheet path stays accessible (test by clicking Start with a queued file → `modal-preflight` should appear if ffmpeg is needed).
- [ ] Restore: `sudo mv /tmp/ffmpeg-backup $(which ffmpeg-was-here)` — remember the original path before moving.

### Task 6.13 — Memory: jot any deviation

- [ ] If any verification step had to be relaxed (e.g. ETA payload field name differs, an `invoke` had to be renamed, etc.), record it in `docs/superpowers/notes/2026-05-02-rust-contract-snapshot.md` under a new `## Deviations during implementation` section. This file already exists from Phase 0.

---

## Phase 7 — Wrap

### Task 7.1 — Bump version

- [ ] Read current versions:
  ```
  grep -nE '^version' /media/owner/Workspace/histv-universal/src-tauri/Cargo.toml
  grep -nE '"version"' /media/owner/Workspace/histv-universal/src-tauri/tauri.conf.json
  ```
- [ ] Expected: both report `2.6.0`.
- [ ] Bump both to `2.7.0-dev.0`:
  - `src-tauri/Cargo.toml`: change `version = "2.6.0"` → `version = "2.7.0-dev.0"`.
  - `src-tauri/tauri.conf.json`: change `"version": "2.6.0"` → `"version": "2.7.0-dev.0"`.
  - **No root `package.json` exists**, so the package.json bump is a no-op (verified Phase 0).
- [ ] Re-run cargo check:
  ```
  cd /media/owner/Workspace/histv-universal/src-tauri && cargo check
  ```
- [ ] Expected: `Finished` (Cargo accepts the pre-release tag).

### Task 7.2 — Note the title-string version mismatch (do NOT block on it)

- [ ] The title in `src/index.html` reads `Honey, I Shrunk The Vids v2.6.0`. After the bump, it is now stale by one minor. **Do not edit it in this PR** — the title traditionally tracks released versions, not -dev tags. Add a TODO note in the deviations section of the snapshot doc instead.

### Task 7.3 — Commit version bump

- [ ] ```
  git -C /media/owner/Workspace/histv-universal add src-tauri/Cargo.toml src-tauri/tauri.conf.json
  git -C /media/owner/Workspace/histv-universal commit -m "chore(release): bump to 2.7.0-dev.0 for UI redesign branch"
  ```

### Task 7.4 — Push the branch

- [ ] ```
  git -C /media/owner/Workspace/histv-universal push -u origin feat/ui-redesign-2026-05
  ```
- [ ] Expected: branch pushed; remote-tracking link established.

### Task 7.5 — Open a draft PR linking the spec

- [ ] ```
  cd /media/owner/Workspace/histv-universal && \
  gh pr create --draft \
    --title "feat(ui): single-canvas + slide-over-sheet redesign (2026-05)" \
    --body "$(cat <<'EOF'
  ## Summary

  - Rewrites the desktop UI around a single queue canvas, a slide-over settings sheet, a quiet status bar, and a top-right (or bottom FAB at compact) action bar.
  - Snaps across three breakpoints (compact <600, medium 600-839, expanded ≥840) — no animated reflow.
  - Drops the Tauri window minimum to 380x600.
  - Adds two optional `AppConfig` fields (`uiSheetOpen`, `uiSheetSection`) with serde defaults; existing configs load unchanged.
  - Keeps every `invoke` and `listen` call intact; preserves every required DOM id.
  - `lib.test.js` extended with `resolveBreakpoint`, `formatQuietStatusPlan`, `formatQuietStatusEncoding` unit tests.

  ## Spec

  See [`docs/superpowers/specs/2026-05-02-ui-redesign-design.md`](docs/superpowers/specs/2026-05-02-ui-redesign-design.md).

  Implementation plan: [`docs/superpowers/plans/2026-05-02-ui-redesign.md`](docs/superpowers/plans/2026-05-02-ui-redesign.md).

  Rust contract snapshot: [`docs/superpowers/notes/2026-05-02-rust-contract-snapshot.md`](docs/superpowers/notes/2026-05-02-rust-contract-snapshot.md).

  ## Test plan

  - [x] `node --test src/js/lib.test.js` (green; +7 new tests)
  - [x] `cargo test --lib` (green; +2 new `test_ui_sheet_*` tests)
  - [x] `cargo build --release`
  - [x] Manual resize sweep 1920 → 380px
  - [x] Keyboard shortcut sweep (spec §6 / §12)
  - [x] Drag-drop + paste at all three breakpoints
  - [x] Theme cycle across all status states
  - [x] Persisted settings + sheet state on relaunch
  - [x] ffmpeg-missing first-run dry test

  ## Follow-ups (not in this PR)

  - Take new screenshots for the README at all three breakpoints.
  - Consider whether the title bar should track -dev tags.
  EOF
  )"
  ```
- [ ] Expected: `gh` prints a PR URL.

---

## Self-review (writing-plans skill)

### Spec coverage map

| Spec section | Plan task(s) |
|---|---|
| §3 Architecture / config addition | Phase 5 (Tasks 5.1-5.6) |
| §4 Breakpoints | Phase 1 (window min); Phase 3 (CSS @media); Phase 4 (matchMedia) |
| §5.1 Queue (canvas) | Task 2.2 (table + cards container); Task 4.9 (render branch) |
| §5.2 Queue row anatomy | Task 4.9 (`renderQueueCards` HTML template) |
| §5.3 Action bar | Task 2.2 (`<nav id="action-bar">`); Task 3.3 (CSS for FAB + cluster) |
| §5.4 Settings sheet | Task 2.2 (`<aside id="settings-sheet">` with seven sections); Task 4.8 (open/close/keyboard) |
| §5.5 Quiet status bar | Task 2.2 (`<footer id="quiet-status-bar">`); Task 4.4-4.5 (formatters); Task 4.10 (updater + click) |
| §5.6 Pre-flight warning sheet | Task 2.2 (`#modal-preflight` retained) |
| §5.7 Encoder probe inline strip | Task 2.2 (`#encoder-probe-strip`); Task 4.11 (handler) |
| §5.8 Empty state | Task 2.2 (`#queue-empty-state`); Task 3.3 (centred styling) |
| §6 Interaction (keyboard, pointer) | Task 4.8 (Ctrl+, / Esc); existing handlers preserved by Task 4.12 grep |
| §7 Theming | Task 3.1-3.2 (token preservation); Task 3.5 (token grep) |
| §8 Status hierarchy | Task 3.3 (`.status-pill.*` rules) |
| §9 Migration / contract surface | Phase 0 snapshot; Task 2.4 id sweep; Task 4.12 contract grep |
| §11 Resolved decisions OQ-1..9 | OQ-1 (sheet 420px → Task 3.3); OQ-2 (compact full-cover, Esc, backdrop → Tasks 3.3, 4.8); OQ-3 (tappable bar → Task 4.10); OQ-4 (snap → Task 3.3 transitions rule); OQ-5 (FAB + Start pill → Task 2.2, 3.3); OQ-6 (gradient overlay → Task 3.3 `.row-progress`); OQ-7 (probe strip + Start disabled → Task 4.11); OQ-8 (380x600 → Phase 1); OQ-9 (theme picker under Appearance → Task 2.2 `#section-appearance`) |
| §12 Testing checklist | Phase 6 (every line of §12 mapped to a 6.x task) |
| §13 Success criteria | Phase 6 in aggregate; explicit no-new-deps rule honoured by writing zero `Cargo.toml` deps and no JS deps |

**Gaps deliberately not addressed in this plan:**
- README screenshot regeneration — explicitly punted to a follow-up (per the "Phase 7 Wrap" instruction in the user brief).
- Audio settings section was called out as "inferred from existing config" by §5.4 and contains no explicit ids in the required-id list. The plan does not add an Audio section to the sheet; if the existing app exposed audio controls under ids in the required-id list, they would be visible — none are listed, so this is consistent with the spec's "no new controls" instruction. If during execution it becomes clear the legacy app had user-visible audio controls, add them to `#settings-sheet` under a new `<section id="section-audio">` and re-run Task 2.4.

### Placeholder scan

- No "TBD", no "implement later", no "similar to Task N". Every code block is complete and exact.

### Type / id consistency

- Every DOM id used in JS tasks (e.g. `#queue-empty-state`, `#sheet-backdrop`, `#btn-open-settings`, `#encoder-probe-strip`) is defined in Task 2.2's HTML.
- Every function name referenced cross-task (`openSettingsSheet`, `closeSettingsSheet`, `renderQueue`, `renderQueueCards`, `renderQueueTableCondensed`, `renderQueueTableFull`, `applyBreakpoint`, `onBreakpointChange`, `updateQuietStatusBar`, `updateQuietStatusBarEncoding`, `resolveBreakpoint`, `formatQuietStatusPlan`, `formatQuietStatusEncoding`, `persistSheetState`) is defined in exactly one task and referenced consistently in others.
- Rust field names (`ui_sheet_open`, `ui_sheet_section`) → JSON keys (`uiSheetOpen`, `uiSheetSection`) are consistent (camelCase serde).

### Commit message style

- All commits use conventional-commits with parenthetical scope, matching observed repo style: `feat(ui)`, `feat(rust)`, `fix(ui)`, `chore(tauri)`, `chore(release)`, `test(ui)`, `docs(ui)`.
