// Mock of the Tauri v2 bridge the HISTV front end expects, injected before app.js.
//
// Reads window.__MOCK_FIXTURES__ (written by the harness before this file):
//   config, themes, queue, encoders, audioEncoders, dialogPaths, preflightWarnings,
//   delays   {command: ms}   artificial latency per command
//   drop     [command]       commands to treat as unknown (negative control)
//
// A command or event this file does not know is never answered with undefined:
// it is recorded in window.__MOCK__.failures, logged with console.error, and the
// call rejects. The harness reads `failures` after every cell and aborts the run.
(() => {
  const F = window.__MOCK_FIXTURES__;
  const clone = (v) => JSON.parse(JSON.stringify(v));
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

  const KNOWN_EVENTS = new Set([
    'encoder-detection-done', 'ffmpeg-missing', 'ffmpeg-download-progress', 'log',
    'ffmpeg-stderr', 'file-progress', 'queue-item-updated', 'queue-item-probed',
    'batch-started', 'batch-progress', 'batch-status', 'batch-command', 'wave-progress',
    'wave-status', 'batch-finished', 'queue-sync-complete', 'overwrite-prompt',
    'fallback-prompt', 'toast', 'post-batch',
    'tauri://drag-enter', 'tauri://drag-leave', 'tauri://drag-drop',
  ]);

  const M = {
    config: clone(F.config),
    themes: clone(F.themes),
    queue: [],   // F.queue holds the rows add_files_to_queue appends
    fixtures: F,
    calls: [],
    failures: [],
    listeners: {},
    settled: {},   // command -> number of completed calls
  };

  function fail(kind, name, detail) {
    const entry = { kind, name, detail: detail || null };
    M.failures.push(entry);
    console.error(`[mock] UNKNOWN ${kind} "${name}"`, detail || '');
    return new Error(`mock-tauri: unknown ${kind} "${name}"`);
  }

  // Each handler returns the value the real command would return.
  const handlers = {
    get_config: () => clone(M.config),
    save_config: ({ config }) => { M.config = clone(config); return null; },
    get_themes: () => clone(M.themes),
    attach_log_listener: () => null,
    get_encoder_detection_status: () => true,
    get_detected_encoders: () => [clone(F.encoders), clone(F.audioEncoders)],
    get_ffmpeg_missing_status: () => false,
    get_queue: () => clone(M.queue),
    // The dialog fixture stands in for the files the user picks; the rows they
    // become are the whole fixture queue, so every status is on screen.
    'plugin:dialog|open': (opts) => (opts && opts.directory ? '/home/user/Videos/output' : clone(F.dialogPaths)),
    add_files_to_queue: () => {
      const startIndex = M.queue.length;
      M.queue = M.queue.concat(clone(F.queue));
      return { startIndex, count: F.queue.length };
    },
    probe_file: () => null,
    remove_queue_items: ({ indices }) => {
      const gone = new Set(indices);
      M.queue = M.queue.filter((_, i) => !gone.has(i));
      return null;
    },
    clear_completed: () => { M.queue = M.queue.filter((q) => q.status !== 'Done'); return null; },
    clear_all_queue: () => { M.queue = []; return null; },
    requeue_items: () => null,
    requeue_all: () => null,
    move_queue_item: () => null,
    open_file: () => null,
    reveal_file: () => null,
    repair_tags: () => 0,
    deep_repair_tags: () => 0,
    preflight_check: () => clone(F.preflightWarnings),
    download_mp4box: () => null,
    download_ffmpeg: () => null,
    start_batch: () => null,
    toggle_pause: () => false,
    cancel_current: () => null,
    cancel_all: () => null,
    respond_overwrite: () => null,
    respond_fallback: () => null,
    execute_post_batch_action: () => null,
    'plugin:notification|is_permission_granted': () => true,
    'plugin:notification|request_permission': () => 'granted',
    'plugin:notification|notify': () => null,
  };
  for (const name of F.drop || []) delete handlers[name];

  async function invoke(cmd, args) {
    M.calls.push({ kind: 'invoke', name: cmd, args: args === undefined ? null : clone(args) });
    const wait = (F.delays || {})[cmd];
    if (wait) await sleep(wait);
    const h = handlers[cmd];
    if (!h) throw fail('invoke', cmd, args);
    const out = await h(args || {});
    M.settled[cmd] = (M.settled[cmd] || 0) + 1;
    return out === undefined ? null : out;
  }

  let nextId = 1;
  async function listen(event, handler) {
    M.calls.push({ kind: 'listen', name: event });
    if (!KNOWN_EVENTS.has(event)) throw fail('listen', event);
    (M.listeners[event] = M.listeners[event] || []).push(handler);
    const id = nextId++;
    return () => { M.listeners[event] = (M.listeners[event] || []).filter((h) => h !== handler); return id; };
  }

  async function emit(event, payload) {
    M.calls.push({ kind: 'emit', name: event });
    return M.fire(event, payload);
  }

  // Backend-to-front-end event. An event nobody listens to is a harness bug
  // (a scenario that cannot work), so it fails loudly too.
  M.fire = async (event, payload) => {
    if (!KNOWN_EVENTS.has(event)) throw fail('fire', event);
    const hs = M.listeners[event] || [];
    if (hs.length === 0) throw fail('fire-without-listener', event);
    await Promise.all(hs.map((h) => h({ event, id: 0, payload: payload === undefined ? null : clone(payload) })));
  };

  // The ETA text depends on wall-clock time between two events; a settable
  // clock makes it the same on every run.
  M.setNow = () => { throw new Error('mock-tauri: fakeClock is off'); };
  if (F.fakeClock) {
    let now = 1000;
    performance.now = () => now;
    M.setNow = (v) => { now = v; };
  }

  window.__MOCK__ = M;
  window.__TAURI__ = { core: { invoke }, event: { listen, emit } };
})();
