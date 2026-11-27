    // ═══════════════════════════════════════════════════════════════
    //  HISTV Tauri Frontend - app.js
    // ═══════════════════════════════════════════════════════════════
    const { invoke } = window.__TAURI__.core;
    const { listen, emit } = window.__TAURI__.event;

    // Dialog helper - wraps the Tauri v2 dialog plugin.
    // With withGlobalTauri: true, plugins may expose their API on a global,
    // or we can invoke the plugin command directly.
    async function dialogOpen(opts) {
      try {
        if (window.__TAURI_PLUGIN_DIALOG__ && window.__TAURI_PLUGIN_DIALOG__.open) {
          return await window.__TAURI_PLUGIN_DIALOG__.open(opts);
        }
      } catch (_) { /* fall through */ }
      return invoke('plugin:dialog|open', opts);
    }

    // ── State ──────────────────────────────────────────────────────
	let queueData = [];                // mirrors backend queue
    let videoEncoders = [];            // EncoderInfo[]
	let batchWasCancelled = false;     // check if batch was user-cancelled
    let themes = [];                   // Theme[]
    let selectedRows = new Set();      // indices of selected rows
    let batchRunning = false;
    let encoderDetectionDone = false;
    let saveDebounce = null;
    let queueRefreshDebounce = null;   // coalesces rapid queue fetch+render cycles
    let targetBitrateDebounce = null;  // coalesces rapid refreshTargetBitrates calls
    let batchCurrentFile = 0;          // current file index in batch (1-based)
    let batchTotalFiles = 0;           // total files in batch
    let currentEncodingIndex = -1;     // queue index of the currently-encoding row
	let currentEncodeProgress = '';    // store the file progress as a global value
    let fileEncodeStart = 0;           // wall-clock timestamp when current file started
    const defaultTitle = document.title;

    // ── DOM refs ───────────────────────────────────────────────────
    const $ = (s) => document.querySelector(s);

    const queueBody       = $('#queue-body');
    const dropOverlay     = $('#drop-overlay');
    const numBitrate      = $('#num-bitrate');
    const peakToggle      = $('#peak-toggle');
    const numQpI          = $('#num-qp-i');
    const numQpP          = $('#num-qp-p');
    const numCrf          = $('#num-crf');
    const rowQp           = $('#row-qp');
    const rowCrf          = $('#row-crf');
    const rowRcMode       = $('#row-rc-mode');
    const rcToggle        = $('#rc-toggle');
    const chkHdr          = $('#chk-hdr');
    const chkPrecision    = $('#chk-precision');
    const numThreads      = $('#num-threads');
    const chkLowPriority  = $('#chk-low-priority');
    const selTheme        = $('#sel-theme');
    const selUiStyle      = $('#sel-ui-style');
    const txtOutputFolder = $('#txt-output-folder');
    const radioOutputMode = document.querySelectorAll('input[name="output-mode"]');
    const chkOverwrite    = $('#chk-overwrite');
    const chkDeleteSource = $('#chk-delete-source');
    const chkSaveLog      = $('#chk-save-log');
    const chkToast        = $('#chk-toast');
    const chkAutoClear    = $('#chk-auto-clear');
    const chkForceLocal   = $('#chk-force-local');
    const selPostAction   = $('#sel-post-action');
    const txtCustomCmd    = $('#txt-custom-command');
    const numCountdown    = $('#num-countdown');
    const rowCustomCmd    = $('#row-custom-command');
    const jobStatus       = $('#job-status');
    const statusCmd       = $('#job-cmd');
    const progressBar     = $('#progress-bar');
    const batchProgressBar = $('#batch-progress-bar');
    const btnStart        = $('#btn-start');
    const startTooltip    = $('#start-tooltip');
    const queueSummary    = $('#queue-summary');
    const btnCancelCur    = $('#btn-cancel-current');
    const btnCancelAll    = $('#btn-cancel-all');
    const btnPause        = $('#btn-pause');
    const rowCountdown    = $('#row-countdown');
    const bottomPanelBody = $('#bottom-panel-body');
    const bottomPanelToggle = $('#bottom-panel-toggle');
    const bottomPanelArrow  = $('#bottom-panel-arrow');
    const logContent      = $('#log-content');
    const jobFile         = $('#job-file');
    const jobProgressText = $('#job-progress-text');
    const contextMenu     = $('#context-menu');
	const encoderSummary  = $('#encoder-summary');
    const chkPreserveAv1  = $('#chk-preserve-av1');
    const chkCompat       = $('#chk-compat');
    const progressBarContainer = $('#progress-bar-container');
    const batchProgressBarContainer = $('#batch-progress-bar-container');
    const liveStatus      = $('#live-status');

    // Status icons. All strokes use currentColor, so each glyph takes the
    // colour of the status badge it sits in.
    const STATUS_ICONS = {
      pending: '',
      probing: '<svg aria-hidden="true" class="spinner" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><circle cx="12" cy="12" r="10" stroke-dasharray="31.4 31.4" stroke-linecap="round"/></svg>',
      encoding: '<svg aria-hidden="true" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="9"/><path d="M12 6v6l4 2"/></svg>',
      done: '<svg aria-hidden="true" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><path d="M5 13l4 4L19 7"/></svg>',
      failed: '<svg aria-hidden="true" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><path d="M6 6l12 12M18 6L6 18"/></svg>',
      skipped: '<svg aria-hidden="true" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M5 5l14 7-14 7V5z"/><line x1="19" y1="5" x2="19" y2="19"/></svg>',
      cancelled: '<svg aria-hidden="true" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><circle cx="12" cy="12" r="9"/><line x1="9" y1="9" x2="15" y2="15"/></svg>',
	};
	
	// Flatpak mode restrictions removed — with --filesystem=host, all
	// features (drag-and-drop, clipboard paste, custom output folder)
	// work identically to the native build.

    // ═══════════════════════════════════════════════════════════════
    //  INITIALISATION
    // ═══════════════════════════════════════════════════════════════
    document.addEventListener('DOMContentLoaded', async () => {
      const savedConfig = await loadConfig();
      await loadThemes(savedConfig);
      setupSplitter();
      setupQueueButtons();
      setupSettingsListeners();
      setupProfiles();
      setupBottomPanel();
      setupLogFilters();
      setupContextMenu();
      setupDragAndDrop();
      setupClipboardPaste();
      setupKeyboardShortcuts();
      setupDragReorder();
      setupColumnResize();
      setupTooltips();
      setupModalButtons();
      setupBottomButtons();
      await listenTauriEvents();
      // Replays log lines emitted before the listener existed
      try {
        await invoke('attach_log_listener');
      } catch (e) {
        console.error('attach_log_listener failed:', e);
      }

      // In case encoder detection completed before listeners were ready
      const alreadyDone = await invoke('get_encoder_detection_status');
      if (alreadyDone) {
        try {
          const [ve] = await invoke('get_detected_encoders');
          videoEncoders = ve;
          updateEncoderSummary();
          encoderDetectionDone = true;
          if (chkPrecision.checked) applyPrecisionMode();
          updateBatchButtons();
        } catch (e) {
          console.warn('Encoder fetch error:', e);
        }
      } else {
        // In case ffmpeg-missing fired before listeners were ready
        const ffmpegMissing = await invoke('get_ffmpeg_missing_status');
        if (ffmpegMissing) {
          encoderSummary.textContent = 'ffmpeg not found';
          const modal = document.querySelector('#modal-ffmpeg-missing');
          modal.classList.add('visible');
          trapFocusInModal(modal);

          document.querySelector('#ffmpeg-dl-yes').onclick = async () => {
            releaseFocusTrap(modal);
            modal.classList.remove('visible');
            encoderSummary.textContent = 'Downloading ffmpeg';
            try {
              await invoke('download_ffmpeg');
            } catch (e) {
              encoderSummary.textContent = 'Download failed: ' + e;
              const body = document.querySelector('#ffmpeg-missing-body');
              body.textContent = 'Download failed: ' + e + ' (install ffmpeg, then restart)';
              modal.classList.add('visible');
            }
          };

          document.querySelector('#ffmpeg-dl-no').onclick = () => {
            releaseFocusTrap(modal);
            modal.classList.remove('visible');
            encoderSummary.textContent = 'ffmpeg not found (install ffmpeg, then restart)';
          };
        }
      }
    });

    // ── Load persisted config from backend ─────────────────────────
    async function loadConfig() {
      try {
        const cfg = await invoke('get_config');
        // js/ui-style.js applied the last known style before first paint; the
        // config is authoritative. loadThemes then derives the theme for it.
        selUiStyle.value = normaliseUiStyle(cfg.uiStyle);
        switchUiStyle(selUiStyle.value);
        numBitrate.value = cfg.targetBitrate || 4;
        setPeakMultiplier(cfg.peakMultiplier || 1.5);
        numQpI.value = cfg.qpI ?? 20;
        numQpP.value = cfg.qpP ?? 22;
        numCrf.value = cfg.crf ?? 20;
        chkHdr.checked = cfg.hdr || false;
        chkPrecision.checked = cfg.precisionMode || false;
		chkPreserveAv1.checked = cfg.preserveAv1 || false;
        chkCompat.checked = cfg.compatibilityMode || false;
        // Enforce mutual exclusion on load (compat wins if both are saved)
        if (chkCompat.checked) {
          chkPreserveAv1.checked = false;
          chkHdr.checked = false;
        }
        numThreads.value = cfg.threads ?? 0;
        chkLowPriority.checked = cfg.lowPriority || false;
        txtOutputFolder.value = cfg.outputFolder || 'output';
        setOutputMode(cfg.outputMode || 'folder');
        chkOverwrite.checked = cfg.overwrite || false;
        chkDeleteSource.checked = cfg.deleteSource || false;
        chkSaveLog.checked = cfg.saveLog || false;
        chkToast.checked = cfg.showToast || false;
        chkAutoClear.checked = cfg.autoClearCompleted || false;
        chkForceLocal.checked = cfg.forceLocal || false;
        selPostAction.value = cfg.postAction || 'None';
        toggleCustomCommand();
        txtCustomCmd.value = cfg.customCommand || '';
        numCountdown.value = cfg.postCountdown || 0;
        if (cfg.logDrawerOpen) {
          bottomPanelBody.classList.add('open');
          bottomPanelArrow.textContent = '\u25BC';
        }
        // Restore rate control mode
        const rcMode = cfg.rateControlMode || 'QP';
        setRateControlMode(rcMode);
        return cfg;
      } catch (e) {
        console.warn('Could not load config:', e);
        selUiStyle.value = normaliseUiStyle(document.documentElement.dataset.uiStyle);
        return null;
      }
    }

    // ── Persist config (debounced) ─────────────────────────────────
    function scheduleConfigSave() {
      if (saveDebounce) clearTimeout(saveDebounce);
      saveDebounce = setTimeout(async () => {
        try {
          await invoke('save_config', {
            config: {
              theme: selTheme.value || 'Default Dark',
              uiStyle: normaliseUiStyle(selUiStyle.value),
              outputFolder: txtOutputFolder.value,
              outputMode: getOutputMode(),
              outputContainer: 'auto',
              overwrite: chkOverwrite.checked,
              deleteSource: chkDeleteSource.checked,
              saveLog: chkSaveLog.checked,
              showToast: chkToast.checked,
              autoClearCompleted: chkAutoClear.checked,
              forceLocal: chkForceLocal.checked,
              postAction: selPostAction.value,
              postCountdown: parseInt(numCountdown.value, 10) || 0,
              customCommand: txtCustomCmd.value,
              targetBitrate: parseInt(numBitrate.value, 10) || 4,
              peakMultiplier: getCurrentPeakMultiplier(),
              qpI: parseInt(numQpI.value, 10) || 20,
              qpP: parseInt(numQpP.value, 10) || 22,
              crf: parseInt(numCrf.value, 10) || 20,
              rateControlMode: getCurrentRcMode(),
              hdr: chkHdr.checked,
              precisionMode: chkPrecision.checked,
			  compatibilityMode: chkCompat.checked,
              preserveAv1: chkPreserveAv1.checked,
              threads: parseInt(numThreads.value, 10) || 0,
              lowPriority: chkLowPriority.checked,
              logDrawerOpen: bottomPanelBody.classList.contains('open'),
            }
          });
        } catch (e) {
          console.warn('Config save failed:', e);
        }
      }, 600);
    }

    // ═══════════════════════════════════════════════════════════════
    //  THEMING (§3)
    // ═══════════════════════════════════════════════════════════════
    async function loadThemes(savedConfig) {
      try {
        // Doublet pairings ship in doublet-tokens.css and head the list;
        // user themes from the themes/ folder follow. A user theme with the
        // same name as a Doublet pairing replaces that pairing.
        const userThemes = await invoke('get_themes');
        const userNames = new Set(userThemes.map(t => t.name));
        themes = DOUBLET_THEMES.filter(d => !userNames.has(d.name)).concat(userThemes);
        selTheme.innerHTML = '';
        for (const t of themes) {
          const opt = document.createElement('option');
          opt.value = t.name;
          opt.textContent = t.name;
          selTheme.appendChild(opt);
        }
        // Restore saved theme from the config already loaded
        if (savedConfig && savedConfig.theme && themes.find(t => t.name === savedConfig.theme)) {
          selTheme.value = savedConfig.theme;
        }
        applyTheme(selTheme.value);
      } catch (e) {
        console.warn('Could not load themes:', e);
      }
    }

// ── Theme colour derivation ─────────────────────────────────
    // A theme is six colours: background (cloth), surface, text, primary
    // (thread), success, error. Everything else is derived here as Doublet
    // tokens (--dx-*) and set inline on :root, over the defaults in
    // doublet-tokens.css. Legacy key names still work as fallbacks.
    //
    // The Doublet pairings need no derivation under the Doublet style: their
    // tokens are already in doublet-tokens.css, selected with data-dx-pairing.
    // Under the Classic style that file is disabled, so each pairing also
    // carries its six seed colours (equal to doublet-tokens.css; lib.test.js
    // checks) and is derived like any other theme.
    //
    // The Classic style derives the pre-Doublet custom properties instead
    // (deriveClassicTokens). The active style is <html data-ui-style>.

    const DOUBLET_THEMES = [
      { name: 'Doublet', pairing: 'electric', colors: {
        background: '#0A0D13', surface: '#12161F', text: '#E9EAEE',
        primary: '#1F6FFF', success: '#2FD07A', error: '#FF4D4D' } },
      { name: 'Doublet Dusk', pairing: 'dusk', colors: {
        background: '#16121E', surface: '#201A2B', text: '#ECE8F2',
        primary: '#FF6A00', success: '#2FD07A', error: '#FF4D4D' } },
      { name: 'Doublet Ember', pairing: 'ember', colors: {
        background: '#0F1620', surface: '#18212E', text: '#E8EAF0',
        primary: '#FF6A00', success: '#2FD07A', error: '#FF4D4D' } },
      { name: 'Doublet Filigree', pairing: 'filigree', colors: {
        background: '#0E0F12', surface: '#181A1F', text: '#E6E8EC',
        primary: '#C9CFD8', success: '#2FD07A', error: '#FF4D4D' } },
    ];

    // Interface style from a config value (mirrors normaliseUiStyle in lib.js).
    function normaliseUiStyle(value) {
      return value === 'classic' ? 'classic' : 'doublet';
    }

    // Which interface style owns an inline custom property on <html> (mirrors
    // uiStyleOfProperty in lib.js, which carries the unit tests).
    function uiStyleOfProperty(prop) {
      if (prop.startsWith('--dx-')) return 'doublet';
      if (/^--(glass|row|progress|icon)-/.test(prop)) return 'classic';
      const classicCore = ['--background', '--surface', '--text', '--primary', '--success', '--error',
        '--surface-dim', '--surface-bright', '--text-muted', '--accent', '--text-on-primary'];
      return classicCore.includes(prop) ? 'classic' : null;
    }

    // Theme JSON keys that feed the derivation; any other key is passed
    // through as a custom property (log-* and future variables). Keys
    // starting with dx- are never passed through: they would override the
    // derived tokens and skip contrast enforcement.
    const THEME_DERIVATION_KEYS = new Set([
      'background', 'surface', 'text', 'primary', 'success', 'error',
      'base-100', 'base-300', 'base-content', 'secondary', 'info',
      'surface-dim', 'surface-bright', 'text-muted', 'accent',
      'row-info', 'row-done', 'row-failed', 'row-cancelled',
      'icon-done', 'icon-failed', 'icon-cancelled',
      'status-done', 'status-failed', 'status-cancelled',
      'text-on-primary',
    ]);

    let appliedThemeProps = [];

    function applyTheme(name) {
      const theme = themes.find(t => t.name === name);
      if (!theme) return;
      const root = document.documentElement;
      const style = normaliseUiStyle(root.dataset.uiStyle);
      const otherStyle = style === 'classic' ? 'doublet' : 'classic';

      // Drop whatever the previous theme, or the previous style, set inline.
      for (const prop of appliedThemeProps) root.style.removeProperty(prop);
      appliedThemeProps = [];
      root.style.removeProperty('color-scheme');
      root.removeAttribute('data-dx-pairing');

      if (style === 'doublet' && theme.pairing) {
        if (theme.pairing !== 'electric') root.setAttribute('data-dx-pairing', theme.pairing);
        return;
      }

      const c = theme.colors || {};
      const setProp = (key, value) => {
        root.style.setProperty(`--${key}`, value);
        appliedThemeProps.push(`--${key}`);
      };

      const tokens = style === 'classic' ? deriveClassicTokens(c) : deriveDoubletTokens(c);
      for (const [key, value] of Object.entries(tokens)) setProp(key, value);

      // Pass through explicit extra keys (with colour validation). Keys the
      // derivation already used, and names of the other style's tokens, are
      // skipped.
      for (const [key, value] of Object.entries(c)) {
        const used = style === 'classic' ? key in tokens : THEME_DERIVATION_KEYS.has(key);
        if (!used && !key.startsWith('dx-') && /^[a-z0-9-]+$/.test(key)
            && uiStyleOfProperty(`--${key}`) !== otherStyle && isValidCssColor(value)) {
          setProp(key, value);
        }
      }

      // Colour-scheme for native form controls
      const ground = style === 'classic' ? (tokens['background'] || '#1E1E1E') : tokens['dx-cloth'];
      root.style.colorScheme = isLightColor(ground) ? 'light' : 'dark';
    }

    function isValidCssColor(v) {
      if (typeof v !== 'string') return false;
      if (/^#[0-9a-fA-F]{3,8}$/.test(v)) return true;
      if (/^rgba?\(\s*\d[\d\s.,%/]*\)$/.test(v)) return true;
      return false;
    }

    // The pre-Doublet (2.6.0) derivation, for the Classic style. Six colours
    // in, the classic custom properties out (named without the leading --).
    // An explicit key in the theme replaces the derived value; a value that
    // is not a valid colour is left out.
    function deriveClassicTokens(c) {
      const bg      = c['background'] || c['base-100'] || '#1E1E1E';
      const surface = c['surface']    || c['base-300'] || '#373737';
      const text    = c['text']       || c['base-content'] || '#DCDCDC';
      const primary = c['primary']    || '#0078D7';
      const success = c['success']    || '#4ade80';
      const error   = c['error']      || '#f87171';
      const light = isLightColor(bg);
      // Muted tint of a bright colour over the background
      const tint = (colour) => blendHex(colour, bg, light ? 0.12 : 0.15);

      const d = {};
      d['background']     = bg;
      d['surface']        = surface;
      d['text']           = text;
      d['primary']        = primary;
      d['success']        = success;
      d['error']          = error;

      // Surface hierarchy
      d['surface-dim']    = c['surface-dim']    || blendHex(surface, bg, 0.5);
      d['surface-bright'] = c['surface-bright'] || shiftBrightness(surface, light ? -0.08 : 0.08);
      d['text-muted']     = c['text-muted']     || c['secondary'] || hexToRgba(text, 0.65);
      d['accent']         = c['accent']         || primary;

      // Row tints
      d['row-info']       = c['row-info']       || c['info']    || blendHex(primary, bg, 0.15);
      d['row-done']       = c['row-done']       || tint(success);
      d['row-encoding']   = c['row-encoding']   || c['warning'] || shiftBrightness(surface, light ? -0.04 : 0.04);
      d['row-failed']     = c['row-failed']     || tint(error);
      d['row-cancelled']  = c['row-cancelled']  || blendHex(d['row-failed'], d['row-encoding'], 0.5);

      // Progress bar
      d['progress-fill']  = c['progress-fill']  || d['row-done'];
      d['progress-bg']    = c['progress-bg']    || d['row-encoding'];

      // Icon / log colours (bright, full saturation)
      d['icon-done']      = c['icon-done']      || c['status-done']      || success;
      d['icon-failed']    = c['icon-failed']    || c['status-failed']    || error;
      d['icon-cancelled'] = c['icon-cancelled'] || c['status-cancelled'] || '#fb923c';
      d['log-result']     = c['log-result']     || d['icon-done'];
      d['log-warn']       = c['log-warn']       || d['icon-cancelled'];
      d['log-error']      = c['log-error']      || d['icon-failed'];
      d['log-detect']     = c['log-detect']     || c['status-detect'] || '#a78bfa';
      d['log-file']       = c['log-file']       || '#7dd3fc';
      d['log-text']       = c['log-text']       || '#8BC88B';
      d['log-cmd']        = c['log-cmd']        || (light ? '#6b7280' : '#7a8090');
      d['text-on-primary'] = c['text-on-primary'] || bestTextOn(primary);

      // Glass tokens, by theme lightness
      Object.assign(d, light ? {
        'glass-bg': 'rgba(255, 255, 255, 0.55)', 'glass-border': 'rgba(0, 0, 0, 0.08)',
        'glass-input-bg': 'rgba(0, 0, 0, 0.04)', 'glass-th-bg': 'rgba(255, 255, 255, 0.8)',
        'glass-hover': 'rgba(0, 0, 0, 0.04)', 'glass-toggle-bg': 'rgba(0, 0, 0, 0.04)',
        'glass-log-bg': 'rgba(20, 20, 20, 0.95)', 'glass-strip-bg': 'rgba(255, 255, 255, 0.6)',
        'glass-overlay-bg': 'rgba(255, 255, 255, 0.92)',
      } : {
        'glass-bg': 'rgba(40, 40, 40, 0.65)', 'glass-border': 'rgba(255, 255, 255, 0.06)',
        'glass-input-bg': 'rgba(55, 55, 55, 0.5)', 'glass-th-bg': 'rgba(55, 55, 55, 0.9)',
        'glass-hover': 'rgba(255, 255, 255, 0.08)', 'glass-toggle-bg': 'rgba(55, 55, 55, 0.4)',
        'glass-log-bg': 'rgba(8, 8, 8, 0.9)', 'glass-strip-bg': 'rgba(40, 40, 40, 0.7)',
        'glass-overlay-bg': 'rgba(40, 40, 40, 0.92)',
      });

      for (const key of Object.keys(d)) {
        if (!isValidCssColor(d[key])) delete d[key];
      }
      return d;
    }

    // Hex to rgba() string
    function hexToRgba(hex, alpha) {
      const [r, g, b] = hexToRgb(hex);
      return `rgba(${r}, ${g}, ${b}, ${alpha})`;
    }

    // Six seeds in, the full --dx-* colour set out. Neutral text and line
    // tokens are moved toward white (black on a light ground) until they reach
    // 4.5:1 / 3:1 on every ground they sit on. Chromatic tokens (thread line,
    // success, error, warn) change OKLCH lightness only, so hue and chroma
    // survive. Explicit overrides in the theme JSON are used as the starting
    // point instead of the derived value.
    function deriveDoubletTokens(c) {
      const pick = (...keys) => {
        for (const k of keys) { const h = hex6(c[k]); if (h) return h; }
        return null;
      };
      const cloth   = pick('background', 'base-100') || '#101014';
      const surface = pick('surface', 'base-300')    || '#24242B';
      const thread  = pick('primary')                || '#0A6CFF';
      const success = pick('success')                || '#22e07a';
      const error   = pick('error')                  || '#ff5a5a';
      const light   = isLightColor(cloth);
      const pole    = light ? '#000000' : '#ffffff';

      const ensure = (hex, grounds, ratio) => {
        for (let t = 0; t < 1; t += 0.04) {
          const candidate = blendHex(pole, hex, t);
          if (grounds.every(g => contrastRatio(candidate, g) >= ratio)) return candidate;
        }
        return pole;
      };
      const ensureHue = (hex, grounds, ratio) => liftLightness(hex, grounds, ratio, light);
      const wash = (hex) => blendHex(hex, cloth, 0.12);

      const surfaceDim    = pick('surface-dim')    || blendHex(surface, cloth, 0.5);
      const surfaceBright = pick('surface-bright') || shiftBrightness(surface, light ? -0.06 : 0.06);
      const warn          = pick('icon-cancelled', 'status-cancelled') || (light ? '#9A4A00' : '#F08A1C');
      const threadWash    = pick('row-info', 'info') || wash(thread);
      const successWash   = pick('row-done')         || wash(success);
      const errorWash     = pick('row-failed')       || wash(error);
      const warnWash      = pick('row-cancelled')    || wash(warn);

      const grounds = [cloth, surfaceDim, surface, surfaceBright, threadWash];
      const text = ensure(pick('text', 'base-content') || '#E6E6EA', grounds, 4.5);

      return {
        'dx-cloth': cloth,
        'dx-surface': surface,
        'dx-text': text,
        'dx-thread': thread,
        'dx-success': success,
        'dx-error': error,
        'dx-surface-dim': surfaceDim,
        'dx-surface-bright': surfaceBright,
        'dx-text-muted': ensure(pick('text-muted', 'secondary') || blendHex(text, cloth, 0.6), grounds, 4.5),
        'dx-line': blendHex(text, cloth, 0.14),
        'dx-line-strong': ensure(blendHex(text, cloth, 0.45), grounds, 3),
        'dx-thread-line': ensureHue(pick('accent') || thread, grounds, 3),
        'dx-thread-deep': blendHex(thread, cloth, 0.5),
        'dx-thread-wash': threadWash,
        'dx-on-thread': pick('text-on-primary') || bestTextOn(thread),
        'dx-success-text': ensureHue(pick('icon-done', 'status-done') || success, grounds.concat(successWash), 4.5),
        'dx-error-text': ensureHue(pick('icon-failed', 'status-failed') || error, grounds.concat(errorWash), 4.5),
        'dx-warn-text': ensureHue(warn, grounds.concat(warnWash), 4.5),
        'dx-success-wash': successWash,
        'dx-error-wash': errorWash,
        'dx-warn-wash': warnWash,
      };
    }

    // Normalise #rgb, #rrggbb or #rrggbbaa to #rrggbb; null for anything else.
    function hex6(v) {
      if (typeof v !== 'string') return null;
      const m = /^#([0-9a-fA-F]{3}|[0-9a-fA-F]{6}|[0-9a-fA-F]{8})$/.exec(v.trim());
      if (!m) return null;
      const h = m[1];
      return '#' + (h.length === 3 ? h.replace(/./g, ch => ch + ch) : h.substring(0, 6));
    }

    // WCAG contrast ratio between two hex colours.
    function contrastRatio(a, b) {
      const la = relLuminance(a), lb = relLuminance(b);
      return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
    }

    // ── OKLab / OKLCH (Ottosson) ─────────────────────────────────
    // Used only to change lightness while keeping hue and chroma.

    function srgbToLinear(v) {
      v /= 255;
      return v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
    }
    function linearToSrgb(v) {
      return 255 * (v <= 0.0031308 ? 12.92 * v : 1.055 * Math.pow(v, 1 / 2.4) - 0.055);
    }

    // Hex to [L, C, h] (L 0..1, C ~0..0.4, h in degrees).
    function hexToOklch(hex) {
      const [r, g, b] = hexToRgb(hex).map(srgbToLinear);
      const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
      const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
      const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
      const L = 0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s;
      const a = 1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s;
      const bb = 0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s;
      return [L, Math.hypot(a, bb), (Math.atan2(bb, a) * 180 / Math.PI + 360) % 360];
    }

    // [L, C, h] to linear sRGB components (may fall outside 0..1).
    function oklchToLinear(L, C, h) {
      const a = C * Math.cos(h * Math.PI / 180), b = C * Math.sin(h * Math.PI / 180);
      const l = Math.pow(L + 0.3963377774 * a + 0.2158037573 * b, 3);
      const m = Math.pow(L - 0.1055613458 * a - 0.0638541728 * b, 3);
      const s = Math.pow(L - 0.0894841775 * a - 1.2914855480 * b, 3);
      return [
        4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
        -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
        -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s,
      ];
    }

    // [L, C, h] to hex. A colour outside sRGB loses chroma (hue and lightness kept).
    function oklchToHex(L, C, h) {
      const inGamut = (c) => oklchToLinear(L, c, h).every(v => v >= -0.0005 && v <= 1.0005);
      let lo = 0, hi = C;
      if (!inGamut(C)) {
        for (let i = 0; i < 24; i++) {
          const mid = (lo + hi) / 2;
          if (inGamut(mid)) lo = mid; else hi = mid;
        }
        C = lo;
      }
      const [r, g, b] = oklchToLinear(L, C, h).map(v => linearToSrgb(Math.max(0, Math.min(1, v))));
      return rgbToHex(r, g, b);
    }

    // Move a colour's OKLCH lightness toward white (black when `light`) until it
    // reaches `ratio`:1 on every ground. Hue is kept; chroma is kept unless the
    // colour leaves sRGB. Returns the input unchanged if it already passes.
    function liftLightness(hex, grounds, ratio, light) {
      const passes = (c) => grounds.every(g => contrastRatio(c, g) >= ratio);
      if (passes(hex)) return hex;
      const [L0, C, h] = hexToOklch(hex);
      const pole = light ? '#000000' : '#ffffff';
      const dir = light ? -1 : 1;
      for (let L = L0 + dir * 0.005; light ? L > 0 : L < 1; L += dir * 0.005) {
        const candidate = oklchToHex(L, C, h);
        if (passes(candidate)) return candidate;
      }
      return pole;
    }

    // ── Colour helper functions ──────────────────────────────────

    // Quick luminance check: returns true if the hex colour is closer
    // to white than black.
    function isLightColor(hex) {
      const c = hex.replace('#', '');
      const r = parseInt(c.substring(0, 2), 16);
      const g = parseInt(c.substring(2, 4), 16);
      const b = parseInt(c.substring(4, 6), 16);
      return (r * 299 + g * 587 + b * 114) / 1000 > 128;
    }

    // Parse hex to [r, g, b]
    function hexToRgb(hex) {
      const c = hex.replace('#', '');
      return [
        parseInt(c.substring(0, 2), 16),
        parseInt(c.substring(2, 4), 16),
        parseInt(c.substring(4, 6), 16),
      ];
    }

    // [r, g, b] to hex
    function rgbToHex(r, g, b) {
      const clamp = v => Math.max(0, Math.min(255, Math.round(v)));
      return '#' + [clamp(r), clamp(g), clamp(b)]
        .map(v => v.toString(16).padStart(2, '0')).join('');
    }

    // Blend two hex colours: result = a * t + b * (1-t)
    function blendHex(a, b, t) {
      const [ar, ag, ab] = hexToRgb(a);
      const [br, bg, bb] = hexToRgb(b);
      return rgbToHex(
        ar * t + br * (1 - t),
        ag * t + bg * (1 - t),
        ab * t + bb * (1 - t),
      );
    }

    // Shift brightness by a factor (-1 to 1). Positive = lighter, negative = darker.
    function shiftBrightness(hex, amount) {
      const [r, g, b] = hexToRgb(hex);
      const shift = amount * 255;
      return rgbToHex(r + shift, g + shift, b + shift);
    }

    // WCAG relative luminance of a hex colour (sRGB).
    function relLuminance(hex) {
      const lin = v => {
        v /= 255;
        return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
      };
      const [r, g, b] = hexToRgb(hex);
      return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
    }

    // Pick black or white for the strongest contrast on a background colour,
    // so text on the primary/accent passes WCAG AA regardless of theme hue.
    function bestTextOn(hex) {
      const L = relLuminance(hex);
      const onWhite = 1.05 / (L + 0.05);
      const onBlack = (L + 0.05) / 0.05;
      return onWhite >= onBlack ? '#ffffff' : '#000000';
    }

    selTheme.addEventListener('change', () => {
      applyTheme(selTheme.value);
      scheduleConfigSave();
    });

    // Interface style: swap the stylesheets once the new ones have loaded
    // (applyUiStyle, js/ui-style.js), then re-derive the theme for the new
    // style in the same step. The theme is unchanged. If a stylesheet does
    // not load, the old style stays and the control goes back to it.
    function switchUiStyle(style, onSwitched) {
      const previous = normaliseUiStyle(document.documentElement.dataset.uiStyle);
      applyUiStyle(style, (failedHref) => {
        if (failedHref) {
          selUiStyle.value = previous;
          const msg = `Style: ${style === 'classic' ? 'Classic' : 'Doublet'} did not load (${failedHref})`;
          appendLog(msg);
          announceStatus(msg);
          return;
        }
        applyTheme(selTheme.value);
        if (onSwitched) onSwitched();
      });
    }

    selUiStyle.addEventListener('change', () => {
      switchUiStyle(normaliseUiStyle(selUiStyle.value), scheduleConfigSave);
    });

    // ═══════════════════════════════════════════════════════════════
    //  SPLITTER (§4.1)
    // ═══════════════════════════════════════════════════════════════
    let settingsPanelCollapsed = false;

    function setupSplitter() {
      const splitter = $('#splitter');
      const splitterToggle = $('#splitter-toggle');
      const rightPanel = $('#right-panel');
      const mainArea = $('#main-area');
      let dragging = false;

      // The rail (#right-panel) sits on the left and owns the width; the queue
      // flexes to fill. The width lives in --rail-w on #main-area, so collapsing
      // keeps it and re-opening restores it. Column mode ignores the variable.
      const QUEUE_MIN_W = 340;      // matches #left-panel min-width
      const SPLITTER_W = 9;         // matches #splitter width
      function clampRail(w) {
        const cs = getComputedStyle(rightPanel);
        const minW = parseInt(cs.minWidth, 10) || 316;
        const maxW = parseInt(cs.maxWidth, 10) || 460;
        const room = mainArea.getBoundingClientRect().width - QUEUE_MIN_W - SPLITTER_W;
        return Math.max(minW, Math.min(maxW, room, w));
      }
      const isColumnMode = () => window.matchMedia('(max-width: 820px)').matches;

      // ── Splitter drag ──
      splitter.addEventListener('mousedown', (e) => {
        if (settingsPanelCollapsed) return;
        if (e.target === splitterToggle || splitterToggle.contains(e.target)) return;
        dragging = true;
        splitter.classList.add('dragging');
        e.preventDefault();
      });

      document.addEventListener('mousemove', (e) => {
        if (!dragging) return;
        const offset = e.clientX - mainArea.getBoundingClientRect().left;
        mainArea.style.setProperty('--rail-w', clampRail(offset) + 'px');
      });

      document.addEventListener('mouseup', () => {
        if (dragging) {
          dragging = false;
          splitter.classList.remove('dragging');
        }
      });

      // ── Toggle button (the rail is on the left: hide points left, show points right) ──
      function setCollapsed(collapsed) {
        settingsPanelCollapsed = collapsed;
        rightPanel.classList.toggle('collapsed', collapsed);
        splitter.classList.toggle('rail-collapsed', collapsed);
        const label = collapsed ? 'Show settings panel' : 'Hide settings panel';
        splitterToggle.innerHTML = collapsed ? '&#x25B6;' : '&#x25C0;';
        splitterToggle.title = label;
        splitterToggle.setAttribute('aria-label', label);
      }
      splitterToggle.addEventListener('click', (e) => {
        e.stopPropagation();
        setCollapsed(!settingsPanelCollapsed);
      });

      // Column mode hides the splitter (and its toggle), so a collapsed rail
      // would be unreachable: entering it re-opens the rail.
      window.matchMedia('(max-width: 820px)').addEventListener('change', (e) => {
        if (e.matches && settingsPanelCollapsed) setCollapsed(false);
      });

      // ── Window resize: keep a dragged rail width inside its limits ──
      let resizeDebounce;
      window.addEventListener('resize', () => {
        clearTimeout(resizeDebounce);
        resizeDebounce = setTimeout(() => {
          const current = parseInt(mainArea.style.getPropertyValue('--rail-w'), 10);
          if (!current || isColumnMode()) return;
          const clamped = clampRail(current);
          if (clamped !== current) mainArea.style.setProperty('--rail-w', clamped + 'px');
        }, 100);
      });
    }

    // ═══════════════════════════════════════════════════════════════
    //  FILE QUEUE (§5)
    // ═══════════════════════════════════════════════════════════════

    // ── Render queue table ──
    // Differential update: only rebuilds rows whose data has changed,
    // and only updates cells within a row if the underlying values differ.
    // Falls back to a full rebuild when the row count changes.
    function renderQueue() {
      const existingRows = queueBody.children;

      // Full rebuild needed if row count changed
      if (existingRows.length !== queueData.length) {
        fullRenderQueue();
        return;
      }

      // Read settings once for target bitrate labels (#6)
      const settings = getTargetBitrateSettings();

      // Differential update - patch only what changed
      for (let i = 0; i < queueData.length; i++) {
        const item = queueData[i];
        const tr = existingRows[i];
        const statusLower = (item.status || 'pending').toLowerCase();

        // Update row class
        const wantClass = `status-${statusLower}` + (selectedRows.has(i) ? ' selected' : '');
        if (tr.className !== wantClass) {
          tr.className = wantClass;
          // Clear any leftover progress fill when status changes
          tr.style.removeProperty('--encode-progress');
        }

        // Cell 0: checkbox
        const cb = tr.children[0] && tr.children[0].firstChild;
        if (cb && cb.checked !== selectedRows.has(i)) {
          cb.checked = selectedRows.has(i);
        }

        // Cell 1: filename
        const c1 = tr.children[1];
        if (c1.textContent !== item.fileName) {
          c1.textContent = item.fileName;
          c1.title = item.fullPath || item.fileName;
        }

        // Cell 2: size
        const sizeText = formatBytes(item.sourceBytes);
        const c2 = tr.children[2];
        if (c2.textContent !== sizeText) c2.textContent = sizeText;

        // Cell 3: compressed (est.)
        const estText = formatEstimatedSize(item, settings);
        const c3 = tr.children[3];
        if (c3.textContent !== estText) c3.textContent = estText;

        // Cell 4: resolution
        const resText = (item.videoWidth && item.videoHeight)
          ? `${item.videoWidth}x${item.videoHeight}` : '';
        const c4 = tr.children[4];
        if (c4.textContent !== resText) c4.textContent = resText;

        // Cell 5: HDR type
        const hdrText = hdrTypeLabel(item);
        const c5 = tr.children[5];
        if (c5.textContent !== hdrText) c5.textContent = hdrText;

        // Cell 6: source bitrate
        const srcText = item.videoBitrateMbps > 0
          ? `${item.videoBitrateMbps.toFixed(2)} Mbps` : '';
        const c6 = tr.children[6];
        if (c6.textContent !== srcText) c6.textContent = srcText;

        // Cell 7: target bitrate
        const tgtText = computeTargetBitrateLabel(item, settings);
        const c7 = tr.children[7];
        if (c7.textContent !== tgtText) c7.textContent = tgtText;

        // Cell 8: status badge
        const c8 = tr.children[8];
        const badge = c8.firstChild;
        if (badge) {
          const currentStatus = tr.dataset.status || '';
          if (currentStatus !== statusLower) {
            const icon = STATUS_ICONS[statusLower] || '';
            badge.innerHTML = '';
            if (icon) {
              const iconSpan = document.createElement('span');
              iconSpan.className = 'status-icon';
              iconSpan.innerHTML = icon;
              badge.appendChild(iconSpan);
            }
            badge.appendChild(document.createTextNode(item.status || 'Pending'));
            tr.dataset.status = statusLower;
          }
        }

        // Keep the compact (narrow-width) summary in sync on the filename cell
        const compact = compactRowSummary(item);
        const nameCell = tr.children[1];
        if (nameCell && nameCell.dataset.compact !== compact) nameCell.dataset.compact = compact;
      }
      updateBatchButtons();
    }

    // Resolution / HDR / source-bitrate, joined for the compact secondary line
    // shown under the filename when those columns collapse at narrow widths.
    function compactRowSummary(item) {
      const parts = [];
      if (item.videoWidth && item.videoHeight) parts.push(`${item.videoWidth}x${item.videoHeight}`);
      const hdr = hdrTypeLabel(item);
      if (hdr) parts.push(hdr);
      if (item.videoBitrateMbps > 0) parts.push(`${item.videoBitrateMbps.toFixed(2)} Mbps`);
      return parts.join(' · ');
    }

    // ── Full queue rebuild (used when row count changes) ──
    function fullRenderQueue() {
	// Toggle empty-state placeholder
      const emptyState = $('#queue-empty-state');
      if (emptyState) {
        emptyState.classList.toggle('visible', queueData.length === 0);
      }
	  
      const fragment = document.createDocumentFragment();
      const settings = getTargetBitrateSettings(); // read DOM once (#6)
      for (let i = 0; i < queueData.length; i++) {
        const item = queueData[i];
        const tr = document.createElement('tr');
        const statusLower = (item.status || 'pending').toLowerCase();
        tr.className = `status-${statusLower}`;
        if (selectedRows.has(i)) tr.classList.add('selected');
        tr.dataset.index = i;
        tr.dataset.status = statusLower;
		if (statusLower === 'encoding' && i === currentEncodingIndex && currentEncodeProgress) {
          tr.style.setProperty('--encode-progress', currentEncodeProgress);
        }

        // Checkbox
        const tdCheck = document.createElement('td');
        tdCheck.className = 'col-check';
        const cb = document.createElement('input');
        cb.type = 'checkbox';
        cb.checked = selectedRows.has(i);
        cb.setAttribute('aria-label', 'Select ' + item.fileName);
        tdCheck.appendChild(cb);
        tr.appendChild(tdCheck);

        // Filename
        const tdName = document.createElement('td');
        tdName.title = item.fullPath || item.fileName;
        tdName.textContent = item.fileName;
        tdName.dataset.compact = compactRowSummary(item);
        tr.appendChild(tdName);
		
		// Size
        const tdSize = document.createElement('td');
        tdSize.textContent = formatBytes(item.sourceBytes);
        tr.appendChild(tdSize);

        // Compressed (est.)
        const tdEst = document.createElement('td');
        tdEst.textContent = formatEstimatedSize(item, settings);
        tr.appendChild(tdEst);

        // Resolution
        const tdRes = document.createElement('td');
        tdRes.textContent = (item.videoWidth && item.videoHeight)
          ? `${item.videoWidth}x${item.videoHeight}` : '';
        tr.appendChild(tdRes);

        // HDR type
        const tdHdr = document.createElement('td');
        tdHdr.textContent = hdrTypeLabel(item);
        tr.appendChild(tdHdr);

        // Source bitrate
        const tdSrc = document.createElement('td');
        tdSrc.textContent = item.videoBitrateMbps > 0
          ? `${item.videoBitrateMbps.toFixed(2)} Mbps` : '';
        tr.appendChild(tdSrc);

        // Target bitrate (§5.7)
        const tdTgt = document.createElement('td');
        tdTgt.textContent = computeTargetBitrateLabel(item, settings);
        tr.appendChild(tdTgt);

        // Status badge
        const tdStatus = document.createElement('td');
        const badge = document.createElement('span');
        badge.className = 'status-badge dx-badge';
        const icon = STATUS_ICONS[statusLower] || '';
        if (icon) {
          const iconSpan = document.createElement('span');
          iconSpan.className = 'status-icon';
          iconSpan.innerHTML = icon;
          badge.appendChild(iconSpan);
        }
        badge.appendChild(document.createTextNode(item.status || 'Pending'));
        tdStatus.appendChild(badge);
        tr.appendChild(tdStatus);
        fragment.appendChild(tr);
      }
      queueBody.innerHTML = '';
      queueBody.appendChild(fragment);

      // Attach a single delegated click handler on the tbody
      // (re-attached on full rebuild since innerHTML clears it)
      queueBody.onclick = handleQueueRowClick;

      // Double-click to open source file in default player
      queueBody.ondblclick = (e) => {
        const tr = e.target.closest('tr');
        if (!tr) return;
        const i = parseInt(tr.dataset.index, 10);
        if (!isNaN(i) && i < queueData.length) {
          invoke('open_file', { path: queueData[i].fullPath }).catch(err => {
            console.error('Double-click open error:', err);
          });
        }
      };

	  updateBatchButtons();
    }

    // ── Delegated row click handler - toggles selection without full rebuild ──
    let reorderJustCompleted = false;

    function handleQueueRowClick(e) {
      // Skip selection change if a reorder drag just ended
      if (reorderJustCompleted) {
        reorderJustCompleted = false;
        return;
      }
      const tr = e.target.closest('tr');
      if (!tr) return;
      const i = parseInt(tr.dataset.index, 10);
      if (isNaN(i)) return;

      const isCheckbox = e.target.tagName === 'INPUT' && e.target.type === 'checkbox';

      const previousSelection = new Set(selectedRows);

      if (e.shiftKey && selectedRows.size > 0) {
        // Safe max without spread - avoids stack overflow at 50k+ items (#9)
        let last = -1;
        for (const r of selectedRows) { if (r > last) last = r; }
        const lo = Math.min(last, i);
        const hi = Math.max(last, i);
        for (let r = lo; r <= hi; r++) selectedRows.add(r);
      } else if (isCheckbox || e.ctrlKey || e.metaKey) {
        // Checkbox click or ctrl+click: toggle individual selection
        if (selectedRows.has(i)) selectedRows.delete(i);
        else selectedRows.add(i);
      } else {
        selectedRows.clear();
        selectedRows.add(i);
      }

      // Only toggle rows whose selection state actually changed (#8)
      const rows = queueBody.children;
      // Rows that were selected before but aren't now
      for (const r of previousSelection) {
        if (!selectedRows.has(r) && r < rows.length) {
          rows[r].classList.remove('selected');
          const cb = rows[r].children[0] && rows[r].children[0].firstChild;
          if (cb) cb.checked = false;
        }
      }
      // Rows that are selected now but weren't before
      for (const r of selectedRows) {
        if (!previousSelection.has(r) && r < rows.length) {
          rows[r].classList.add('selected');
          const cb = rows[r].children[0] && rows[r].children[0].firstChild;
          if (cb) cb.checked = true;
        }
      }

      // Sync select-all checkbox
      syncSelectAll();
    }

    const selectAllCheckbox = $('#select-all');

    function syncSelectAll() {
      if (queueData.length === 0) {
        selectAllCheckbox.checked = false;
        selectAllCheckbox.indeterminate = false;
      } else if (selectedRows.size === queueData.length) {
        selectAllCheckbox.checked = true;
        selectAllCheckbox.indeterminate = false;
      } else if (selectedRows.size > 0) {
        selectAllCheckbox.checked = false;
        selectAllCheckbox.indeterminate = true;
      } else {
        selectAllCheckbox.checked = false;
        selectAllCheckbox.indeterminate = false;
      }
    }

    selectAllCheckbox.addEventListener('change', () => {
      const rows = queueBody.children;
      if (selectAllCheckbox.checked) {
        for (let i = 0; i < queueData.length; i++) {
          selectedRows.add(i);
          if (rows[i]) {
            rows[i].classList.add('selected');
            const cb = rows[i].children[0] && rows[i].children[0].firstChild;
            if (cb) cb.checked = true;
          }
        }
      } else {
        selectedRows.clear();
        for (let i = 0; i < rows.length; i++) {
          rows[i].classList.remove('selected');
          const cb = rows[i].children[0] && rows[i].children[0].firstChild;
          if (cb) cb.checked = false;
        }
      }
      selectAllCheckbox.indeterminate = false;
    });

    // ── Cached settings for target bitrate computation (#6) ──
    function getTargetBitrateSettings() {
      return {
        target: parseFloat(numBitrate.value) || 4,
        peakMultiplier: getCurrentPeakMultiplier(),
        qpI: parseInt(numQpI.value, 10) || 20,
        qpP: parseInt(numQpP.value, 10) || 22,
        crf: parseInt(numCrf.value, 10) || 20,
        rcMode: getCurrentRcMode(),
        compat: chkCompat.checked,
        preserveAv1: chkPreserveAv1.checked,
      };
    }
	
	// ── HDR type label from queue item ──
    function hdrTypeLabel(item) {
      if (item.doviProfile != null) {
        return 'DV' + item.doviProfile;
      }
      if (item.hasHdr10plus) return 'HDR10+';
      if (item.isHdr) {
        const ct = (item.colorTransfer || '').toLowerCase();
        if (ct === 'arib-std-b67') return 'HLG';
        return 'HDR10';
      }
      return 'SDR';
    }

	// ── Human-readable file size ──
    function formatBytes(n) {
      if (!n || n <= 0) return '-';
      if (n >= 1000000000) return (n / 1000000000).toFixed(2) + ' GB';
      return (n / 1000000).toFixed(1) + ' MB';
    }

    // ── Target bitrate label (§5.7) ──
    function computeTargetBitrateLabel(item, settings) {
      const mbps = item.videoBitrateMbps;
      const sourceCodec = (item.videoCodec || '').toLowerCase();
      // Codecs that cannot be stream-copied into MKV/MP4
      const isNonCopyable = ['gif', 'apng', 'mjpeg', 'webp'].includes(sourceCodec);

      if (!mbps || mbps <= 0) {
        if (settings.rcMode === 'CRF') {
          return `CRF ${settings.crf}`;
        }
        return `CQP (${settings.qpI}/${settings.qpP})`;
      }

      const isSameCodec = sourceCodec === resolveFamilyForCodec(sourceCodec, settings);

      if (mbps > settings.target * 1.15 || (mbps > settings.target && !isSameCodec)) {
        const peak = (settings.target * settings.peakMultiplier).toFixed(1).replace(/\.0$/, '');
        return `${settings.target}/${peak}Mbps (VBR)`;
      } else if (isNonCopyable) {
        if (settings.rcMode === 'CRF') {
          return `CRF ${settings.crf}`;
        }
        return `CQP (${settings.qpI}/${settings.qpP})`;
      } else if (mbps > 0) {
        return 'Copy';
      } else {
        if (settings.rcMode === 'CRF') {
          return `CRF ${settings.crf}`;
        }
        return `CQP (${settings.qpI}/${settings.qpP})`;
      }
    }

// ── Estimated output size (§2.4.0 Phase 1) ──
    // Returns { bytes: number, approx: boolean } or null if not estimable.
    // approx=true means CQP/CRF: prefix display with "~".
    function computeEstimatedSize(item, settings) {
      if (!item.sourceBytes || item.sourceBytes <= 0) return null;

      const mbps = item.videoBitrateMbps;
      const sourceCodec = (item.videoCodec || '').toLowerCase();
      const isNonCopyable = ['gif', 'apng', 'mjpeg', 'webp'].includes(sourceCodec);

      // Have file size but no probe data yet
      if (!mbps || mbps <= 0) {
        if (!item.videoCodec) return null;
        // Probed but zero bitrate (image sequence) - quality-based encode
        return { bytes: item.sourceBytes, approx: true };
      }

      const isSameCodec = sourceCodec === resolveFamilyForCodec(sourceCodec, settings);

      if (mbps > settings.target * 1.15 || (mbps > settings.target && !isSameCodec)) {
        // VBR decision
        const estimatedBytes = (settings.target * 1000000 * item.durationSecs) / 8;
        return { bytes: Math.round(estimatedBytes), approx: false };
      } else if (isNonCopyable) {
        // CQP/CRF for non-copyable codecs
        return { bytes: item.sourceBytes, approx: true };
      } else if (mbps > 0) {
        // Copy decision
        return { bytes: item.sourceBytes, approx: false };
      }

      return null;
    }

    // ── Format estimated size for display ──
    function formatEstimatedSize(item, settings) {
      const est = computeEstimatedSize(item, settings);
      if (!est) return '-';
      const formatted = formatBytes(est.bytes);
      return est.approx ? '~' + formatted : formatted;
    }

    // ── Update all target bitrate labels (#11: uses .children not querySelectorAll) ──
    function refreshTargetBitrates() {
      const rows = queueBody.children;
      const settings = getTargetBitrateSettings();
      for (let i = 0; i < rows.length && i < queueData.length; i++) {
        const td = rows[i].children[7];
        if (td) td.textContent = computeTargetBitrateLabel(queueData[i], settings);
      }
    }

// ── Update all estimated size labels (same trigger path as target bitrates) ──
    function refreshEstimatedSizes() {
      const rows = queueBody.children;
      const settings = getTargetBitrateSettings();
      for (let i = 0; i < rows.length && i < queueData.length; i++) {
        const td = rows[i].children[3];
        if (td) td.textContent = formatEstimatedSize(queueData[i], settings);
      }
    }

    // ── Debounced variant - coalesces rapid settings changes into one DOM walk ──
    function scheduleRefreshTargetBitrates() {
      if (targetBitrateDebounce) clearTimeout(targetBitrateDebounce);
      targetBitrateDebounce = setTimeout(() => {
        refreshTargetBitrates();
        refreshEstimatedSizes();
      }, 50);
    }

    // ── Debounced queue refresh - coalesces rapid fetch+render cycles ──
    // Multiple sources (probeFileAt completions, Tauri events) can all
    // request a queue refresh within milliseconds of each other. This
    // collapses them into a single IPC call + render.
    function scheduleQueueRefresh() {
      if (queueRefreshDebounce) clearTimeout(queueRefreshDebounce);
      queueRefreshDebounce = setTimeout(async () => {
        await fetchQueue();
        renderQueue();
        checkHdrAuto();
		checkAv1Auto();
      }, 80);
	}

    // ── Debounced render-only (no IPC) ──
    // Used by delta events (queue-item-probed) that already patch
    // queueData locally — we just need a single coalesced renderQueue()
    // call rather than one per event. Separate timer from
    // scheduleQueueRefresh so a pending delta-render doesn't cancel
    // an in-flight full refresh.
    let queueRenderDebounce = null;
    function scheduleQueueRender() {
      if (queueRenderDebounce) clearTimeout(queueRenderDebounce);
      queueRenderDebounce = setTimeout(() => {
        renderQueue();
        checkHdrAuto();
        checkAv1Auto();
      }, 80);
    }
	  
	  async function fetchQueue() {
      queueData = await invoke('get_queue');
      // The backend queue is a snapshot from before encoding started.
      // During a batch, the encoding item's status is only tracked on the
      // frontend via queue-item-updated events. Re-apply it after every
      // backend fetch so renders show the correct status.
      if (batchRunning && currentEncodingIndex >= 0 && currentEncodingIndex < queueData.length) {
        queueData[currentEncodingIndex].status = 'Encoding';
      }
      refreshEncoderDisplay();
    }

    // ── Add files via backend ──
    async function addFilesToQueue(paths) {
      if (!paths || paths.length === 0) return;
      try {
        const { startIndex, count } = await invoke('add_files_to_queue', { paths });
        // Re-fetch full queue to stay in sync
        await fetchQueue();
        renderQueue();
        // Probe newly-added items by index range - no path lookups (#2)
        for (let i = startIndex; i < startIndex + count; i++) {
          if (i < queueData.length && (queueData[i].status || '').toLowerCase() === 'pending') {
            probeFileAt(i);
          }
        }
      } catch (e) {
        console.error('addFilesToQueue error:', e);
      }
    }

    // ── Probe a single file (async, non-blocking) ──
    // The backend emits a queue-item-probed event on both success and
    // failure paths (#3c), carrying the post-probe row. That listener
    // patches queueData[index] and schedules the render, so we don't
    // need to refetch here. On invoke error (Rust command-level
    // failure, not probe failure), schedule a full refresh as a
    // belt-and-braces fallback in case the event never fired.
    async function probeFileAt(index) {
      try {
        await invoke('probe_file', { index });
      } catch (e) {
        console.warn(`Probe failed for index ${index}:`, e);
        scheduleQueueRefresh();
      }
    }

    // ── Auto-check HDR if any file is HDR (§6.1) ──
    function checkHdrAuto() {
      const anyHdr = queueData.some(item => item.isHdr);
      if (anyHdr && !chkHdr.checked) {
        chkHdr.checked = true;
        scheduleConfigSave();
      }
    }
	
	// ── Auto-check Preserve AV1 if any file is AV1 ──
    function checkAv1Auto() {
      const anyAv1 = queueData.some(item => (item.videoCodec || '').toLowerCase() === 'av1');
      if (anyAv1 && !chkPreserveAv1.checked && !chkCompat.checked) {
        chkPreserveAv1.checked = true;
        scheduleConfigSave();
      }
      refreshEncoderDisplay();
    }
	
    // ── Queue buttons ──
    function setupQueueButtons() {
      $('#btn-add-files').addEventListener('click', async () => {
        try {
          const selected = await dialogOpen({
            multiple: true,
            filters: [{
              name: 'Video / GIF Files',
              extensions: ['mkv','mp4','avi','ts','m2ts','wmv','mov','webm','m4v','gif']
            }]
          });
          if (selected) {
            const paths = Array.isArray(selected) ? selected : [selected];
            await addFilesToQueue(paths);
          }
        } catch (e) {
          console.error('File dialog error:', e);
        }
      });

      $('#btn-clear-all').addEventListener('click', async () => {
        if (batchRunning) return;
        try {
          await invoke('clear_all_queue');
          queueData = [];
          selectedRows.clear();
          renderQueue();
          refreshEncoderDisplay();
        } catch (e) {
          console.error('Clear all error:', e);
        }
      });
    }

    // ── Drag-and-drop (§5.1) ──
    function setupDragAndDrop() {
      const container = $('#queue-container');
      let dragCounter = 0;

      // Tauri v2 file drop events
      listen('tauri://drag-enter', () => {
        dragCounter++;
        dropOverlay.classList.add('visible');
      });

      listen('tauri://drag-leave', () => {
        dragCounter--;
        if (dragCounter <= 0) {
          dragCounter = 0;
          dropOverlay.classList.remove('visible');
        }
      });

      listen('tauri://drag-drop', async (event) => {
        dragCounter = 0;
        dropOverlay.classList.remove('visible');
        const paths = event.payload?.paths || event.payload;
        if (Array.isArray(paths) && paths.length > 0) {
          await addFilesToQueue(paths);
        }
      });

      // Browser-level fallback for platforms where Tauri native drag
      // events don't fire
      document.addEventListener('dragover', (e) => {
        e.preventDefault();
        e.dataTransfer.dropEffect = 'copy';
      });

      document.addEventListener('dragenter', () => {
        dragCounter++;
        dropOverlay.classList.add('visible');
      });

      document.addEventListener('dragleave', () => {
        dragCounter--;
        if (dragCounter <= 0) {
          dragCounter = 0;
          dropOverlay.classList.remove('visible');
        }
      });

      document.addEventListener('drop', (e) => {
        e.preventDefault();
        dragCounter = 0;
        dropOverlay.classList.remove('visible');
      });
    }

    // ── Clipboard paste (§5.1 / §15.4) ──
    function setupClipboardPaste() {
      document.addEventListener('keydown', async (e) => {
        if ((e.ctrlKey || e.metaKey) && e.key === 'v') {
          // Only paste into the queue area
          const active = document.activeElement;
          if (active && (active.tagName === 'INPUT' || active.tagName === 'TEXTAREA')) return;
          e.preventDefault();
          try {
            const text = await navigator.clipboard.readText();
            if (text) {
              const paths = text.split(/[\r\n]+/).map(s => s.trim()).filter(Boolean);
              if (paths.length > 0) await addFilesToQueue(paths);
            }
          } catch (err) {
            console.warn('Clipboard paste failed:', err);
          }
        }
      });
    }

    // ── Keyboard shortcuts (§5.8) ──
    function setupKeyboardShortcuts() {
      document.addEventListener('keydown', async (e) => {
        // Ignore if focused on an input
        const active = document.activeElement;
        if (active && (active.tagName === 'INPUT' || active.tagName === 'TEXTAREA' || active.tagName === 'SELECT')) return;

        // Delete / Backspace - remove selected
        if ((e.key === 'Delete' || e.key === 'Backspace') && !batchRunning && selectedRows.size > 0) {
          e.preventDefault();
          try {
            await invoke('remove_queue_items', { indices: Array.from(selectedRows) });
            selectedRows.clear();
            await fetchQueue();
            renderQueue();
          } catch (err) {
            console.error('Delete key remove error:', err);
          }
          return;
        }

        // Ctrl+A - select all
        if ((e.ctrlKey || e.metaKey) && e.key === 'a') {
          e.preventDefault();
          selectedRows.clear();
          for (let i = 0; i < queueData.length; i++) selectedRows.add(i);
          // All rows need selected class - skip the has() check
          const rows = queueBody.children;
          for (let r = 0; r < rows.length; r++) {
            rows[r].classList.add('selected');
          }
          return;
        }

        // Ctrl+R - re-queue selected
        if ((e.ctrlKey || e.metaKey) && e.key === 'r' && !batchRunning && selectedRows.size > 0) {
          e.preventDefault();
          try {
            await invoke('requeue_items', { indices: Array.from(selectedRows) });
            await fetchQueue();
            renderQueue();
          } catch (err) {
            console.error('Requeue shortcut error:', err);
          }
          return;
        }
      });
    }

    // ── Drag reorder (§5.9) - mouse-event based ──
    // Tauri's native drag-drop handler intercepts HTML5 drag events, so
    // row reordering uses mousedown/mousemove/mouseup instead.
    let reorderActive = false;
    let reorderSourceIndex = null;
    let reorderStartY = 0;
    let reorderLastIndicator = null; // { index, position: 'above'|'below' }
    const REORDER_THRESHOLD = 5; // pixels before drag activates

    // ── Column resize handles (§5.9) ──
    function setupColumnResize() {
      const ths = document.querySelectorAll('#queue-table thead th');
      ths.forEach((th, i) => {
        // Skip checkbox column and filename column (flexible)
        if (i === 0 || i === 1) return;
        const handle = document.createElement('div');
        handle.className = 'col-resize-handle';
        th.appendChild(handle);

        let startX, startW;
        handle.addEventListener('mousedown', (e) => {
          e.preventDefault();
          e.stopPropagation();
          startX = e.clientX;
          startW = th.offsetWidth;
          handle.classList.add('active');
          // table-layout:fixed propagates th width to all td cells automatically
          const onMove = (ev) => {
            th.style.width = Math.max(30, startW + ev.clientX - startX) + 'px';
          };
          const onUp = () => {
            handle.classList.remove('active');
            document.removeEventListener('mousemove', onMove);
            document.removeEventListener('mouseup', onUp);
          };
          document.addEventListener('mousemove', onMove);
          document.addEventListener('mouseup', onUp);
        });
      });
    }

    function setupDragReorder() {
      queueBody.addEventListener('mousedown', (e) => {
        if (batchRunning || e.button !== 0) return;
        const tr = e.target.closest('tr');
        if (!tr) return;
        reorderSourceIndex = parseInt(tr.dataset.index, 10);
        if (isNaN(reorderSourceIndex)) { reorderSourceIndex = null; return; }
        reorderStartY = e.clientY;
        reorderActive = false;
        reorderLastIndicator = null;
      });

      document.addEventListener('mousemove', (e) => {
        if (reorderSourceIndex === null) return;
        const rows = queueBody.children;

        // Activate after passing the drag threshold
        if (!reorderActive) {
          if (Math.abs(e.clientY - reorderStartY) < REORDER_THRESHOLD) return;
          reorderActive = true;
          if (rows[reorderSourceIndex]) {
            rows[reorderSourceIndex].classList.add('drag-source');
          }
        }

        // Clear only the previous indicator (#7)
        if (reorderLastIndicator) {
          const prevRow = rows[reorderLastIndicator.index];
          if (prevRow) prevRow.classList.remove('drag-over-above', 'drag-over-below');
        }

        // Find which gap the cursor is closest to.
        // We calculate the insertion point as: above row N means "insert before N",
        // below row N means "insert after N". We skip showing anything on the
        // source row's own position since moving it there is a no-op.
        let closestRow = null;
        let position = null;

        for (let i = 0; i < rows.length; i++) {
          const rect = rows[i].getBoundingClientRect();
          if (e.clientY < rect.top || e.clientY > rect.bottom) continue;

          const midY = rect.top + rect.height / 2;
          if (e.clientY < midY) {
            // Cursor is in the top half - insertion line goes above this row
            // That means "insert before row i"
            // Skip if this would be a no-op (inserting before the source or after the row above it)
            if (i === reorderSourceIndex || i === reorderSourceIndex + 1) break;
            closestRow = i;
            position = 'above';
          } else {
            // Cursor is in the bottom half - insertion line goes below this row
            // That means "insert after row i"
            // Skip if no-op (inserting after the source or before the row below it)
            if (i === reorderSourceIndex || i === reorderSourceIndex - 1) break;
            closestRow = i;
            position = 'below';
          }
          break;
        }

        // Handle cursor above/below the entire table
        if (closestRow === null && rows.length > 0) {
          const firstRect = rows[0].getBoundingClientRect();
          const lastRect = rows[rows.length - 1].getBoundingClientRect();
          if (e.clientY < firstRect.top && reorderSourceIndex !== 0) {
            closestRow = 0;
            position = 'above';
          } else if (e.clientY > lastRect.bottom && reorderSourceIndex !== rows.length - 1) {
            closestRow = rows.length - 1;
            position = 'below';
          }
        }

        if (closestRow !== null && position) {
          rows[closestRow].classList.add(position === 'above' ? 'drag-over-above' : 'drag-over-below');
          reorderLastIndicator = { index: closestRow, position };
        } else {
          reorderLastIndicator = null;
        }
      });

      document.addEventListener('mouseup', async (e) => {
        if (reorderSourceIndex === null) return;

        const wasActive = reorderActive;
        const fromIndex = reorderSourceIndex;
        const indicator = reorderLastIndicator;

        // Clean up visual state - only the affected rows (#7)
        const rows = queueBody.children;
        if (rows[fromIndex]) {
          rows[fromIndex].classList.remove('drag-source');
        }
        if (indicator && rows[indicator.index]) {
          rows[indicator.index].classList.remove('drag-over-above', 'drag-over-below');
        }
        reorderSourceIndex = null;
        reorderActive = false;
        reorderLastIndicator = null;

        if (!wasActive || !indicator) return;

        // Prevent the click handler from firing after a reorder drag
        reorderJustCompleted = true;

        // Calculate the target index from the indicator position.
        // "above row N" = insert at index N (pushing N down)
        // "below row N" = insert at index N+1
        let toIndex = indicator.position === 'above' ? indicator.index : indicator.index + 1;

        // Adjust for the removal of the source - when the source is before
        // the target, removing it shifts everything above the target down by one.
        if (fromIndex < toIndex) toIndex--;

        // Clamp
        toIndex = Math.max(0, Math.min(toIndex, queueData.length - 1));

        if (fromIndex !== toIndex) {
          try {
            await invoke('move_queue_item', { from: fromIndex, to: toIndex });
            selectedRows.clear();
            await fetchQueue();
            renderQueue();
          } catch (err) {
            console.error('Move item error:', err);
          }
        }
      });
    }

    // ═══════════════════════════════════════════════════════════════
    //  SETTINGS LISTENERS
    // ═══════════════════════════════════════════════════════════════
    function setupSettingsListeners() {
      // Codec/mode change → refresh encoder summary, target bitrates, RC visibility
	  
      // Mutual exclusion: toggle off conflicting options (never gray out)
      chkPreserveAv1.addEventListener('change', () => {
        if (chkPreserveAv1.checked) {
          chkCompat.checked = false;
        }
        updateEncoderSummary();
        updateRcModeVisibility();
        scheduleRefreshTargetBitrates();
        scheduleConfigSave();
      });

      chkCompat.addEventListener('change', () => {
        if (chkCompat.checked) {
          chkPreserveAv1.checked = false;
          chkHdr.checked = false;
        }
        updateEncoderSummary();
        updateRcModeVisibility();
        scheduleRefreshTargetBitrates();
        scheduleConfigSave();
      });
	  
      numBitrate.addEventListener('change', () => {
        scheduleRefreshTargetBitrates();
        scheduleConfigSave();
      });

      for (const btn of peakToggle.querySelectorAll('button')) {
        btn.addEventListener('click', () => {
          setPeakMultiplier(btn.dataset.peak);
          scheduleRefreshTargetBitrates();
          scheduleConfigSave();
        });
      }

      numQpI.addEventListener('change', () => {
        scheduleRefreshTargetBitrates();
        scheduleConfigSave();
      });

      numQpP.addEventListener('change', () => {
        scheduleRefreshTargetBitrates();
        scheduleConfigSave();
      });

      numCrf.addEventListener('change', () => {
        scheduleRefreshTargetBitrates();
        scheduleConfigSave();
      });

      chkHdr.addEventListener('change', () => {
        scheduleConfigSave();
      });

      chkPrecision.addEventListener('change', applyPrecisionMode);

      // RC mode toggle (QP / CRF)
      for (const btn of rcToggle.querySelectorAll('button')) {
        btn.addEventListener('click', () => {
          setRateControlMode(btn.dataset.mode);
          scheduleRefreshTargetBitrates();
          scheduleConfigSave();
        });
      }

      // Output mode radio group
      for (const radio of radioOutputMode) {
        radio.addEventListener('change', () => {
          updateOutputModeUI();
          scheduleConfigSave();
        });
      }

      // Simple save-on-change for remaining controls
      const simpleSaveControls = [
        txtOutputFolder,
        chkOverwrite, chkDeleteSource, chkSaveLog, chkToast, chkAutoClear,
        chkPrecision, numThreads, chkLowPriority, selPostAction, txtCustomCmd, numCountdown
      ];
      for (const el of simpleSaveControls) {
        el.addEventListener('change', scheduleConfigSave);
      }

      // Post-action: show/hide custom command row
      selPostAction.addEventListener('change', toggleCustomCommand);
    }

    function toggleCustomCommand() {
      const isNone = selPostAction.value === 'None';
      rowCustomCmd.style.display = selPostAction.value === 'Custom Command' ? 'flex' : 'none';
      rowCountdown.style.display = isNone ? 'none' : 'flex';
    }

    function getOutputMode() {
      const checked = document.querySelector('input[name="output-mode"]:checked');
      return checked ? checked.value : 'folder';
    }

    function setOutputMode(mode) {
      for (const radio of radioOutputMode) {
        radio.checked = radio.value === mode;
      }
      updateOutputModeUI();
    }

    function updateOutputModeUI() {
      const mode = getOutputMode();
      const disableFolder = mode !== 'folder';
      txtOutputFolder.disabled = disableFolder;
      txtOutputFolder.style.opacity = disableFolder ? '0.4' : '1';
      $('#btn-browse-output').disabled = disableFolder;
      $('#btn-browse-output').style.opacity = disableFolder ? '0.4' : '1';
    }

    // ── Rate control mode helpers ──
    function getCurrentRcMode() {
      const activeBtn = rcToggle.querySelector('button.active');
      return activeBtn ? activeBtn.dataset.mode : 'QP';
    }

    function setRateControlMode(mode) {
      for (const btn of rcToggle.querySelectorAll('button')) {
        const isActive = btn.dataset.mode === mode;
        btn.classList.toggle('active', isActive);
        btn.setAttribute('aria-pressed', isActive);
      }
      if (mode === 'CRF') {
        rowQp.style.display = 'none';
        rowCrf.style.display = 'flex';
      } else {
        rowQp.style.display = 'flex';
        rowCrf.style.display = 'none';
      }
    }

    function isSoftwareEncoder(name) {
      return name === 'libx265' || name === 'libx264' || name === 'libsvtav1';
    }

    // ── Peak multiplier toggle helpers ──
    function getCurrentPeakMultiplier() {
      const activeBtn = peakToggle.querySelector('button.active');
      return activeBtn ? parseFloat(activeBtn.dataset.peak) : 1.5;
    }

    function setPeakMultiplier(value) {
      const str = String(value);
      for (const btn of peakToggle.querySelectorAll('button')) {
        const isActive = btn.dataset.peak === str;
        btn.classList.toggle('active', isActive);
        btn.setAttribute('aria-pressed', isActive);
      }
    }

    // ── Quality profiles ──────────────────────────────────────
    // The average user picks a named profile instead of touching raw
    // bitrate/QP/CRF. Each profile just sets the existing controls, so the
    // start_batch payload and config persistence are unchanged. Editing any
    // advanced control switches the active profile to "Custom".
    function setupProfiles() {
      const profileList = $('#profile-list');
      const advancedToggle = $('#advanced-toggle');
      const advancedArrow = $('#advanced-arrow');
      const settingsPanel = $('#settings-panel');
      if (!profileList || !settingsPanel || !advancedToggle) return;
      const cards = Array.from(profileList.querySelectorAll('.profile-card'));

      // Profiles map to engine settings. QP is used (works on hardware and
      // software); CRF stays an Advanced/Custom choice. targetBitrate is the
      // shrink threshold: files above it are shrunk to it, files below are
      // re-encoded for quality at the QP values.
      const PROFILES = {
        'archive-high':     { bitrate: 12, rc: 'QP', qpI: 18, qpP: 20, peak: '3',   compat: false, av1: true,  hdr: true },
        'archive-balanced': { bitrate: 6,  rc: 'QP', qpI: 20, qpP: 22, peak: '3',   compat: false, av1: true,  hdr: true },
        'space-saver':      { bitrate: 3,  rc: 'QP', qpI: 24, qpP: 26, peak: '1.5', compat: false, av1: true,  hdr: true },
        'max-compat':       { bitrate: 6,  rc: 'QP', qpI: 22, qpP: 24, peak: '3',   compat: true,  av1: false, hdr: false },
      };

      let applying = false;
      let advancedOpen = false;

      function setActiveCard(name) {
        for (const c of cards) {
          const on = c.dataset.profile === name;
          c.classList.toggle('active', on);
          c.setAttribute('aria-pressed', on ? 'true' : 'false');
        }
      }

      function showAdvanced(open) {
        advancedOpen = open;
        settingsPanel.hidden = !open;
        advancedToggle.setAttribute('aria-expanded', open ? 'true' : 'false');
        if (advancedArrow) advancedArrow.innerHTML = open ? '&#x25BC;' : '&#x25B6;';
      }

      function applyProfile(name) {
        if (name === 'custom') {
          setActiveCard('custom');
          showAdvanced(true);
          try { localStorage.setItem('histv-profile', 'custom'); } catch (e) {}
          return;
        }
        const p = PROFILES[name];
        if (!p) return;
        applying = true;
        numBitrate.value = p.bitrate;
        setRateControlMode(p.rc);
        numQpI.value = p.qpI;
        numQpP.value = p.qpP;
        setPeakMultiplier(p.peak);
        chkCompat.checked = p.compat;
        chkPreserveAv1.checked = p.av1;
        chkHdr.checked = p.hdr;
        chkPrecision.checked = false;
        applying = false;
        updateRcModeVisibility();
        updateEncoderSummary();
        scheduleRefreshTargetBitrates(); // refresh queue est./target columns for the new profile
        scheduleConfigSave();
        setActiveCard(name);
        try { localStorage.setItem('histv-profile', name); } catch (e) {}
      }

      function markCustom() {
        if (applying) return;
        setActiveCard('custom');
        try { localStorage.setItem('histv-profile', 'custom'); } catch (e) {}
      }

      for (const c of cards) {
        c.addEventListener('click', () => applyProfile(c.dataset.profile));
      }
      advancedToggle.addEventListener('click', () => showAdvanced(!advancedOpen));

      // Editing any advanced control -> Custom (guarded against profile apply).
      for (const el of [numBitrate, numQpI, numQpP, numCrf, numThreads]) {
        if (el) el.addEventListener('input', markCustom);
      }
      for (const el of [chkHdr, chkPreserveAv1, chkCompat, chkPrecision]) {
        if (el) el.addEventListener('change', markCustom);
      }
      for (const btn of rcToggle.querySelectorAll('button')) btn.addEventListener('click', markCustom);
      for (const btn of peakToggle.querySelectorAll('button')) btn.addEventListener('click', markCustom);

      // Init: respect last choice; smart-default to Balanced on first launch.
      let saved = null;
      try { saved = localStorage.getItem('histv-profile'); } catch (e) {}
      if (saved === 'custom') {
        setActiveCard('custom');           // keep loadConfig-restored values
      } else if (saved && PROFILES[saved]) {
        applyProfile(saved);
      } else {
        applyProfile('archive-balanced');  // smart default for new users
      }
    }

    // Codec family the backend encodes one source to (mirrors resolveFamilyForCodec
    // in lib.js, which carries the unit tests).
    function resolveFamilyForCodec(codec, opts) {
      if (opts.compat) return 'h264';
      const c = (codec || '').toLowerCase();
      if (c === 'h264') return 'h264';
      if (c === 'hevc' || c === 'h265') return 'hevc';
      if (opts.preserveAv1 && c === 'av1') return 'av1';
      return 'hevc';
    }

    // Codec families the backend will encode to for the files still to be
    // processed (mirrors resolveEncoderFamilies in lib.js).
    function resolveEncoderFamilies() {
      const opts = { compat: chkCompat.checked, preserveAv1: chkPreserveAv1.checked };
      if (opts.compat) return ['h264'];
      const active = queueData.filter(it =>
        ['pending', 'probing', 'encoding'].includes((it.status || '').toLowerCase()));
      if (active.length === 0) return ['hevc'];
      const found = new Set(active.map(it => resolveFamilyForCodec(it.videoCodec, opts)));
      return ['hevc', 'h264', 'av1'].filter(f => found.has(f));
    }

    // Resolve the active encoder(s) for the current settings and queue. Used
    // by both the encoder summary display and the RC mode visibility logic.
    function resolveActiveEncoders() {
      return resolveEncoderFamilies().map(family => {
        const swFallback = family === 'h264' ? 'libx264' : family === 'av1' ? 'libsvtav1' : 'libx265';
        let encoder;
        if (chkPrecision.checked) {
          encoder = swFallback;
        } else {
          const candidates = videoEncoders.filter(e => e.codecFamily === family);
          const match = candidates.find(e => e.isHardware) || candidates[0];
          encoder = match ? match.name : swFallback;
        }
        return { family, encoder };
      });
    }

    function updateRcModeVisibility() {
      if (resolveActiveEncoders().some(({ encoder }) => isSoftwareEncoder(encoder))) {
        rowRcMode.style.display = 'flex';
      } else {
        if (!chkPrecision.checked) {
          rowRcMode.style.display = 'none';
          setRateControlMode('QP');
        }
      }
    }

    // Re-evaluate the encoder display after the queue changed, but only when
    // the set of target families differs from the last one shown.
    let lastEncoderFamiliesKey = '';
    function refreshEncoderDisplay() {
      if (!encoderDetectionDone) return;
      const key = resolveEncoderFamilies().join(',');
      if (key === lastEncoderFamiliesKey) return;
      lastEncoderFamiliesKey = key;
      updateEncoderSummary();
      updateRcModeVisibility();
      scheduleRefreshTargetBitrates();
    }

    /// When precision mode is toggled, force software encoder + CRF, or
    /// restore the previous selection when untoggled.
    let prePrecisionRcMode = '';
    function applyPrecisionMode() {
      if (chkPrecision.checked) {
        prePrecisionRcMode = getCurrentRcMode();
        // Force CRF mode
        rowRcMode.style.display = 'flex';
        setRateControlMode('CRF');
        for (const btn of rcToggle.querySelectorAll('button')) {
          btn.disabled = true;
          btn.style.opacity = '0.4';
        }
      } else {
        for (const btn of rcToggle.querySelectorAll('button')) {
          btn.disabled = false;
          btn.style.opacity = '1';
        }
        if (prePrecisionRcMode) {
          setRateControlMode(prePrecisionRcMode);
        }
        updateRcModeVisibility();
      }
      updateEncoderSummary();
      scheduleRefreshTargetBitrates();
      scheduleConfigSave();
    }

    // ═══════════════════════════════════════════════════════════════
    //  ENCODER DETECTION (§8)
    // ═══════════════════════════════════════════════════════════════
    // ── Encoder summary line (§2.4.0) ──
    function updateEncoderSummary() {
      lastEncoderFamiliesKey = resolveEncoderFamilies().join(',');
      const parts = resolveActiveEncoders().map(({ family, encoder }) => {
        const isHw = !encoder.startsWith('lib');
        const familyLabel = family === 'h264' ? 'H.264' : family === 'av1' ? 'AV1' : 'HEVC';
        return `${encoder} (${isHw ? 'HW' : 'SW'}), ${familyLabel}`;
      });
      encoderSummary.textContent = `Encoder: ${parts.join('; ')}`;
    }

    // ═══════════════════════════════════════════════════════════════
    //  BOTTOM PANEL TOGGLE & RESIZE (§11)
    // ═══════════════════════════════════════════════════════════════
    function setupBottomPanel() {
      // Toggle open/close on click
      bottomPanelToggle.addEventListener('click', () => {
        const isOpen = bottomPanelBody.classList.toggle('open');
        bottomPanelArrow.textContent = isOpen ? '\u25BC' : '\u25B2';
        bottomPanelToggle.setAttribute('aria-expanded', isOpen ? 'true' : 'false');
        scheduleConfigSave();
      });
    }

    let lastLogMessage = '';
    let lastLogElement = null;
    let lastLogCount = 0;
    let logScrollPending = false;
    const MAX_LOG_LINES = 5000;

    // ── Log line classification ──
    function classifyLog(msg) {
      const trimmed = msg.trimStart();
      if (trimmed.startsWith('CMD:') || trimmed.startsWith('CMD (fallback):'))
        return { cls: 'log-line-cmd', filter: 'cmd' };
      if (trimmed.startsWith('[detect]'))
        return { cls: 'log-line-detect', filter: 'detect' };
      if (trimmed.includes('ERROR'))
        return { cls: 'log-line-error', filter: 'error' };
      if (trimmed.includes('WARNING'))
        return { cls: 'log-line-warn', filter: 'warn' };
      if (trimmed.startsWith('Done →') || trimmed.startsWith('Remuxed') || trimmed.startsWith('Batch done') || trimmed.startsWith('Batch cancelled') || trimmed.startsWith('Batch stopped'))
        return { cls: 'log-line-result', filter: 'result' };
      if (/^\[\d+\/\d+\]/.test(trimmed))
        return { cls: 'log-line-file', filter: '' };
      return { cls: '', filter: '' };
    }

    // Active log filters - categories that are currently hidden
    let logHiddenFilters = new Set();

    function appendLog(msg) {
      // Deduplicate consecutive identical messages
      if (msg === lastLogMessage && lastLogElement) {
        lastLogCount++;
        let counter = lastLogElement.querySelector('.log-repeat-count');
        if (!counter) {
          counter = document.createElement('span');
          counter.className = 'log-repeat-count';
          lastLogElement.appendChild(counter);
        }
        counter.textContent = `(x${lastLogCount})`;
        scheduleLogScroll();
        return;
      }

      lastLogMessage = msg;
      lastLogCount = 1;

      const { cls, filter } = classifyLog(msg);
      const line = document.createElement('div');
      line.textContent = msg;
      if (cls) line.classList.add(cls);
      if (filter) {
        line.dataset.logFilter = filter;
        if (logHiddenFilters.has(filter)) line.classList.add('log-hidden');
      }
      logContent.appendChild(line);
      lastLogElement = line;

      // Cap log at MAX_LOG_LINES (#4)
      while (logContent.childElementCount > MAX_LOG_LINES) {
        logContent.removeChild(logContent.firstChild);
      }

      scheduleLogScroll();
    }

    // ── Log filter button handling ──
    function setupLogFilters() {
      const filterBtns = document.querySelectorAll('.log-filter-btn');
      filterBtns.forEach(btn => {
        btn.addEventListener('click', () => {
          const filter = btn.dataset.filter;

          if (filter === 'all') {
            const allActive = btn.classList.contains('active');
            filterBtns.forEach(b => {
              if (allActive) {
                b.classList.remove('active');
                if (b.dataset.filter !== 'all') logHiddenFilters.add(b.dataset.filter);
              } else {
                b.classList.add('active');
                logHiddenFilters.delete(b.dataset.filter);
              }
            });
          } else {
            btn.classList.toggle('active');
            if (btn.classList.contains('active')) {
              logHiddenFilters.delete(filter);
            } else {
              logHiddenFilters.add(filter);
            }
            const allBtn = document.querySelector('.log-filter-btn[data-filter="all"]');
            const categoryBtns = [...filterBtns].filter(b => b.dataset.filter !== 'all');
            const allOn = categoryBtns.every(b => b.classList.contains('active'));
            allBtn.classList.toggle('active', allOn);
          }

          // Sync aria-pressed on all filter buttons
          filterBtns.forEach(b => b.setAttribute('aria-pressed', b.classList.contains('active')));

          const lines = logContent.children;
          for (let i = 0; i < lines.length; i++) {
            const f = lines[i].dataset.logFilter;
            if (f) {
              lines[i].classList.toggle('log-hidden', logHiddenFilters.has(f));
            }
          }
        });
      });
    }

    // Batch scroll updates via requestAnimationFrame (#4)
    function scheduleLogScroll() {
      if (logScrollPending) return;
      logScrollPending = true;
      requestAnimationFrame(() => {
        logContent.scrollTop = logContent.scrollHeight;
        logScrollPending = false;
      });
    }

    // ═══════════════════════════════════════════════════════════════
    //  TOOLTIPS (JS-positioned, not clipped by overflow)
    // ═══════════════════════════════════════════════════════════════
    function setupTooltips() {
      const popup = $('#tooltip-popup');
      let hideTimeout = null;

      function showFor(icon) {
        const container = icon.closest('.has-tooltip');
        if (!container) return;
        const textEl = container.querySelector('.tooltip-text');
        if (!textEl) return;

        if (hideTimeout) { clearTimeout(hideTimeout); hideTimeout = null; }

        popup.textContent = textEl.textContent;
        popup.classList.add('visible');

        const iconRect = icon.getBoundingClientRect();
        const popupRect = popup.getBoundingClientRect();

        let top = iconRect.top - popupRect.height - 6;
        if (top < 4) top = iconRect.bottom + 6;

        let left = iconRect.left;
        const maxLeft = window.innerWidth - popupRect.width - 8;
        if (left > maxLeft) left = maxLeft;
        if (left < 4) left = 4;

        const rightPanel = $('#right-panel');
        if (rightPanel) {
          const rpRect = rightPanel.getBoundingClientRect();
          popup.style.maxWidth = Math.max(150, rpRect.width - 16) + 'px';
        }

        popup.style.top = top + 'px';
        popup.style.left = left + 'px';
      }
      function scheduleHide() {
        hideTimeout = setTimeout(() => popup.classList.remove('visible'), 100);
      }

      // Make help icons keyboard-focusable so the tooltip text is reachable
      // without a mouse (a11y). Idempotent over the static .tooltip-icon set.
      for (const icon of document.querySelectorAll('.tooltip-icon')) {
        icon.setAttribute('tabindex', '0');
        icon.setAttribute('role', 'button');
        if (!icon.getAttribute('aria-label')) icon.setAttribute('aria-label', 'Help');
      }

      // Pointer + keyboard, via event delegation.
      document.addEventListener('mouseover', (e) => {
        const icon = e.target.closest('.tooltip-icon');
        if (icon) showFor(icon);
      });
      document.addEventListener('mouseout', (e) => {
        if (e.target.closest('.tooltip-icon')) scheduleHide();
      });
      document.addEventListener('focusin', (e) => {
        const icon = e.target.closest('.tooltip-icon');
        if (icon) showFor(icon);
      });
      document.addEventListener('focusout', (e) => {
        if (e.target.closest('.tooltip-icon')) scheduleHide();
      });
      document.addEventListener('keydown', (e) => {
        if (e.key === 'Escape') popup.classList.remove('visible');
      });
    }

    // ═══════════════════════════════════════════════════════════════
    //  CONTEXT MENU (§5.5)
    // ═══════════════════════════════════════════════════════════════
    function setupContextMenu() {
      const container = $('#queue-container');

      // Shared logic for showing the context menu at a given position
      function showContextMenu(x, y) {
        const hasSelection = selectedRows.size > 0;
        const hasTerminal = queueData.some(i => {
          const s = (i.status || '').toLowerCase();
          return s === 'done' || s === 'failed' || s === 'skipped' || s === 'cancelled';
        });
        const selectedHasTerminal = hasSelection && Array.from(selectedRows).some(idx => {
          if (idx >= queueData.length) return false;
          const s = (queueData[idx].status || '').toLowerCase();
          return s === 'done' || s === 'failed' || s === 'skipped' || s === 'cancelled';
        });

        const setCtxState = (id, disabled) => {
          const el = $(id);
          el.classList.toggle('disabled', disabled);
          el.setAttribute('aria-disabled', String(disabled));
          el.setAttribute('tabindex', disabled ? '-1' : '0');
        };
        setCtxState('#ctx-requeue-selected', batchRunning || !selectedHasTerminal);
        setCtxState('#ctx-requeue-all', batchRunning || !hasTerminal);
        setCtxState('#ctx-remove-selected', batchRunning || !hasSelection);
        setCtxState('#ctx-clear-completed', batchRunning);
        setCtxState('#ctx-clear-all', batchRunning);
        setCtxState('#ctx-open-source', !hasSelection);
        setCtxState('#ctx-reveal-source', !hasSelection);
        setCtxState('#ctx-repair-tags', batchRunning || !hasSelection);
        setCtxState('#ctx-deep-repair', batchRunning || !hasSelection);

        contextMenu.style.left = x + 'px';
        contextMenu.style.top = y + 'px';
        contextMenu.classList.add('visible');

        // Focus the first enabled item
        const first = contextMenu.querySelector('.ctx-item:not(.disabled)');
        if (first) first.focus();
      }

      function hideContextMenu() {
        contextMenu.classList.remove('visible');
      }

      container.addEventListener('contextmenu', (e) => {
        e.preventDefault();
        showContextMenu(e.clientX, e.clientY);
      });

      // Keyboard activation: Shift+F10 or ContextMenu key
      container.addEventListener('keydown', (e) => {
        if (e.key === 'ContextMenu' || (e.key === 'F10' && e.shiftKey)) {
          e.preventDefault();
          const rect = container.getBoundingClientRect();
          showContextMenu(rect.left + rect.width / 2, rect.top + rect.height / 2);
        }
      });

      // Arrow navigation within the open menu
      contextMenu.addEventListener('keydown', (e) => {
        const items = [...contextMenu.querySelectorAll('.ctx-item:not(.disabled)')];
        if (!items.length) return;
        const current = document.activeElement;
        const idx = items.indexOf(current);

        if (e.key === 'ArrowDown') {
          e.preventDefault();
          items[idx < items.length - 1 ? idx + 1 : 0].focus();
        } else if (e.key === 'ArrowUp') {
          e.preventDefault();
          items[idx > 0 ? idx - 1 : items.length - 1].focus();
        } else if (e.key === 'Escape') {
          e.preventDefault();
          hideContextMenu();
          container.focus();
        } else if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          if (current && current.classList.contains('ctx-item')) current.click();
        }
      });

      document.addEventListener('click', (e) => {
        if (!contextMenu.contains(e.target)) hideContextMenu();
      });

      // Re-queue selected
      $('#ctx-requeue-selected').addEventListener('click', async () => {
        if (batchRunning || selectedRows.size === 0) return;
        try {
          await invoke('requeue_items', { indices: Array.from(selectedRows) });
          await fetchQueue();
          renderQueue();
        } catch (e) {
          console.error('Requeue selected error:', e);
        }
      });

      // Re-queue all
      $('#ctx-requeue-all').addEventListener('click', async () => {
        if (batchRunning) return;
        try {
          await invoke('requeue_all');
          await fetchQueue();
          renderQueue();
        } catch (e) {
          console.error('Requeue all error:', e);
        }
      });

      // Remove selected
      $('#ctx-remove-selected').addEventListener('click', async () => {
        if (batchRunning || selectedRows.size === 0) return;
        try {
          await invoke('remove_queue_items', { indices: Array.from(selectedRows) });
          selectedRows.clear();
          await fetchQueue();
          renderQueue();
        } catch (e) {
          console.error('Remove selected error:', e);
        }
      });

      // Clear completed (Done only)
      $('#ctx-clear-completed').addEventListener('click', async () => {
        if (batchRunning) return;
        try {
          await invoke('clear_completed');
          selectedRows.clear();
          await fetchQueue();
          renderQueue();
        } catch (e) {
          console.error('Clear completed error:', e);
        }
      });

      // Clear all
      $('#ctx-clear-all').addEventListener('click', async () => {
        if (batchRunning) return;
        try {
          await invoke('clear_all_queue');
          queueData = [];
          selectedRows.clear();
          renderQueue();
          refreshEncoderDisplay();
        } catch (e) {
          console.error('Clear all error:', e);
        }
      });

      // Open source file (first selected)
      $('#ctx-open-source').addEventListener('click', async () => {
        if (selectedRows.size === 0) return;
        const idx = Math.min(...selectedRows);
        if (idx < queueData.length) {
          try {
            await invoke('open_file', { path: queueData[idx].fullPath });
          } catch (e) {
            console.error('Open file error:', e);
          }
        }
      });

      // Reveal source in file manager (first selected)
      $('#ctx-reveal-source').addEventListener('click', async () => {
        if (selectedRows.size === 0) return;
        const idx = Math.min(...selectedRows);
        if (idx < queueData.length) {
          try {
            await invoke('reveal_file', { path: queueData[idx].fullPath });
          } catch (e) {
            console.error('Reveal file error:', e);
          }
        }
      });

      // Repair MKV tags on selected files
      $('#ctx-repair-tags').addEventListener('click', async () => {
        if (batchRunning || selectedRows.size === 0) return;
        const indices = [...selectedRows];
        try {
          const count = await invoke('repair_tags', { indices });
          if (count > 0) {
            await fetchQueue();
            renderQueue();
          }
        } catch (e) {
          console.error('Repair tags error:', e);
        }
      });

      // Deep repair MKV tags on selected files (packet scan)
      $('#ctx-deep-repair').addEventListener('click', async () => {
        if (batchRunning || selectedRows.size === 0) return;
        const indices = [...selectedRows];
        try {
          const count = await invoke('deep_repair_tags', { indices });
          if (count > 0) {
            await fetchQueue();
            renderQueue();
          }
        } catch (e) {
          console.error('Deep repair tags error:', e);
        }
      });
    }

    // ═══════════════════════════════════════════════════════════════
    //  BOTTOM BAR BUTTONS (§12)
    // ═══════════════════════════════════════════════════════════════
    function setupBottomButtons() {
      btnStart.addEventListener('click', startBatch);
      btnCancelCur.addEventListener('click', () => invoke('cancel_current'));
      btnCancelAll.addEventListener('click', () => { batchWasCancelled = true; invoke('cancel_all'); });
      btnPause.addEventListener('click', togglePause);

      // Browse output folder
      $('#btn-browse-output').addEventListener('click', async () => {
        try {
          const folder = await dialogOpen({ directory: true });
          if (folder) {
            txtOutputFolder.value = folder;
            scheduleConfigSave();
          }
        } catch (e) {
          console.error('Browse output error:', e);
        }
      });
    }

    function updateBatchButtons() {
      // Single pass over queueData for both button state and summary (#10)
      let hasPending = false;
      const counts = { pending: 0, done: 0, failed: 0, skipped: 0, cancelled: 0, encoding: 0, probing: 0 };
      for (let i = 0; i < queueData.length; i++) {
        const s = (queueData[i].status || 'pending').toLowerCase();
        if (s === 'pending') hasPending = true;
        if (counts.hasOwnProperty(s)) counts[s]++;
      }

      btnStart.disabled = !encoderDetectionDone || batchRunning || !hasPending;
      btnCancelCur.disabled = !batchRunning;
      btnCancelAll.disabled = !batchRunning;
      btnPause.disabled = !batchRunning;

      // Start button tooltip - explain why it's disabled
      if (btnStart.disabled) {
        if (!encoderDetectionDone) {
          startTooltip.textContent = 'Waiting for encoder detection…';
        } else {
          startTooltip.textContent = '';
        }
      } else {
        startTooltip.textContent = '';
      }

      // Queue summary (same pass data)
      const total = queueData.length;
      if (total === 0) {
        queueSummary.textContent = '';
        return;
      }
      const parts = [];
      if (counts.pending > 0)   parts.push(`${counts.pending} pending`);
      if (counts.encoding > 0)  parts.push(`${counts.encoding} encoding`);
      if (counts.probing > 0)   parts.push(`${counts.probing} probing`);
      if (counts.done > 0)      parts.push(`${counts.done} done`);
      if (counts.failed > 0)    parts.push(`${counts.failed} failed`);
      if (counts.skipped > 0)   parts.push(`${counts.skipped} skipped`);
      if (counts.cancelled > 0) parts.push(`${counts.cancelled} cancelled`);
      queueSummary.textContent = `(${total} - ${parts.join(', ')})`;
    }

    // ── Start batch (§10.1) ──
    async function startBatch(skipDeleteConfirm = false, skipPreflight = false) {
      if (chkDeleteSource.checked && !skipDeleteConfirm) {
        const confirmed = await showDeleteConfirm();
        if (!confirmed) return;
      }

      // Pre-flight DV/HDR10+ check
      if (!skipPreflight) {
        try {
          const warnings = await invoke('preflight_check', { compatibilityMode: chkCompat.checked, preserveHdr: chkHdr.checked });
          if (warnings && warnings.length > 0) {
            const action = await showPreflightModal(warnings);
            if (action === 'cancel') return;
            if (action === 'download') {
              try {
                appendLog('[preflight] Downloading MP4Box...');
                await invoke('download_mp4box');
                appendLog('[preflight] MP4Box installed - starting encode');
                // Re-run preflight; if clear, fall through to start
                const recheck = await invoke('preflight_check', { compatibilityMode: chkCompat.checked, preserveHdr: chkHdr.checked });
                if (recheck && recheck.length > 0) {
                  appendLog('[preflight] Warning: some files still affected after download');
                }
              } catch (e) {
                appendLog('[preflight] MP4Box download failed: ' + e);
                return;
              }
            }
            // action === 'continue': fall through
          }
        } catch (e) {
          console.error('Preflight check error:', e);
        }
      }

      // Check for unknown/empty codecs in pending items
      const knownCodecs = ['h264', 'hevc', 'h265', 'av1', 'mpeg2video', 'mpeg4', 'vc1',
        'vp8', 'vp9', 'prores', 'theora', 'gif', 'apng', 'mjpeg', 'webp',
        'msmpeg4v3', 'msmpeg4v2', 'wmv3', 'wmv2', 'wmv1', 'flv1', 'rv40', 'rv30'];
      const unknownFiles = queueData.filter(item => {
        if ((item.status || '').toLowerCase() !== 'pending') return false;
        const codec = (item.videoCodec || '').toLowerCase();
        return !codec || !knownCodecs.includes(codec);
      });

      if (unknownFiles.length > 0) {
        const names = unknownFiles.slice(0, 10).map(f => f.fileName);
        let body = names.join('\n');
        if (unknownFiles.length > 10) body += `\n+${unknownFiles.length - 10} more`;
        const target = chkCompat.checked ? 'H.264/MP4' : 'HEVC/MKV';
        body += `\n\nOutput: ${target}`;
        $('#modal-unknown-codec-title').textContent = `Unreadable video codec: ${unknownFiles.length} ${unknownFiles.length === 1 ? 'file' : 'files'}`;
        $('#unknown-codec-body').textContent = body;
        $('#unknown-codec-body').style.whiteSpace = 'pre-line';
        const ucModal = $('#modal-unknown-codec');
        ucModal.classList.add('visible');
        trapFocusInModal(ucModal);
        return;
      }

      doStartBatch();
    }

    async function doStartBatch() {
      try {
        await invoke('start_batch', {
          settings: {
            outputFolder: txtOutputFolder.value,
            outputMode: getOutputMode(),
            outputContainer: 'auto',
            targetBitrate: parseFloat(numBitrate.value) || 4,
            peakMultiplier: getCurrentPeakMultiplier(),
            qpI: parseInt(numQpI.value, 10) || 20,
            qpP: parseInt(numQpP.value, 10) || 22,
            crf: parseInt(numCrf.value, 10) || 20,
            rateControlMode: getCurrentRcMode(),
            pixFmt: chkHdr.checked ? 'p010le' : 'yuv420p',
            precisionMode: chkPrecision.checked,
            compatibilityMode: chkCompat.checked,
            preserveAv1: chkPreserveAv1.checked,
            threads: parseInt(numThreads.value, 10) || 0,
            lowPriority: chkLowPriority.checked,
            overwrite: chkOverwrite.checked,
            deleteSource: chkDeleteSource.checked,
            saveLog: chkSaveLog.checked,
            showToast: chkToast.checked,
            postAction: selPostAction.value,
            postCountdown: parseInt(numCountdown.value, 10) || 0,
            forceLocal: chkForceLocal.checked,
          }
        });
      } catch (e) {
        const errMsg = typeof e === 'string' ? e : (e.message || JSON.stringify(e));
        appendLog('ERROR: ' + errMsg);
        jobStatus.textContent = 'Error: ' + errMsg;
      }
    }

    // ── Pause / Resume (§10.5) ──
    async function togglePause() {
      try {
        const paused = await invoke('toggle_pause');
        btnPause.textContent = paused ? 'Resume' : 'Pause';
      } catch (e) {
        console.error('Toggle pause error:', e);
      }
    }

    // ═══════════════════════════════════════════════════════════════
    //  ACCESSIBILITY HELPERS
    // ═══════════════════════════════════════════════════════════════

    function announceStatus(text) {
      if (liveStatus) liveStatus.textContent = text;
    }

    let previouslyFocusedElement = null;

    function trapFocusInModal(modalEl) {
      // Avoid stacking a second keydown handler if this modal is re-opened
      // while already trapped (e.g. the ffmpeg-missing retry path).
      if (modalEl._trapHandler) modalEl.removeEventListener('keydown', modalEl._trapHandler);
      previouslyFocusedElement = document.activeElement;
      const focusables = modalEl.querySelectorAll(
        'button:not([disabled]), input:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex="-1"])'
      );
      if (focusables.length > 0) focusables[0].focus();

      modalEl._trapHandler = (e) => {
        if (e.key !== 'Tab') return;
        const first = focusables[0];
        const last = focusables[focusables.length - 1];
        if (e.shiftKey) {
          if (document.activeElement === first) { e.preventDefault(); last.focus(); }
        } else {
          if (document.activeElement === last) { e.preventDefault(); first.focus(); }
        }
      };
      modalEl.addEventListener('keydown', modalEl._trapHandler);
    }

    function releaseFocusTrap(modalEl) {
      if (modalEl._trapHandler) {
        modalEl.removeEventListener('keydown', modalEl._trapHandler);
        delete modalEl._trapHandler;
      }
      if (previouslyFocusedElement && previouslyFocusedElement.focus) {
        previouslyFocusedElement.focus();
        previouslyFocusedElement = null;
      }
    }

    // ═══════════════════════════════════════════════════════════════
    //  MODAL DIALOGS
    // ═══════════════════════════════════════════════════════════════
    function setupModalButtons() {
      // Overwrite prompt (§7.3)
      $('#ow-yes').addEventListener('click', () => {
        invoke('respond_overwrite', { response: 'yes' });
        releaseFocusTrap($('#modal-overwrite'));
        $('#modal-overwrite').classList.remove('visible');
      });
      $('#ow-no').addEventListener('click', () => {
        invoke('respond_overwrite', { response: 'no' });
        releaseFocusTrap($('#modal-overwrite'));
        $('#modal-overwrite').classList.remove('visible');
      });
      $('#ow-always').addEventListener('click', () => {
        invoke('respond_overwrite', { response: 'always' });
        releaseFocusTrap($('#modal-overwrite'));
        $('#modal-overwrite').classList.remove('visible');
      });

	  // Unknown codec modal
      $('#uc-continue').addEventListener('click', () => {
        releaseFocusTrap($('#modal-unknown-codec'));
        $('#modal-unknown-codec').classList.remove('visible');
        doStartBatch();
      });
      $('#uc-cancel').addEventListener('click', () => {
        releaseFocusTrap($('#modal-unknown-codec'));
        $('#modal-unknown-codec').classList.remove('visible');
      });

      // Fallback prompt (§10.6)
      $('#fb-yes').addEventListener('click', () => {
        invoke('respond_fallback', { response: 'yes' });
        releaseFocusTrap($('#modal-fallback'));
        $('#modal-fallback').classList.remove('visible');
      });
      $('#fb-no').addEventListener('click', () => {
        invoke('respond_fallback', { response: 'no' });
        releaseFocusTrap($('#modal-fallback'));
        $('#modal-fallback').classList.remove('visible');
      });

      // Delete-source confirmation
      // Handled via promise below

      // Countdown cancel
      $('#cd-cancel').addEventListener('click', () => {
        clearCountdownTimer();
        releaseFocusTrap($('#modal-countdown'));
        $('#modal-countdown').classList.remove('visible');
      });

      // Global Escape key handler for modals and context menu
      document.addEventListener('keydown', (e) => {
        if (e.key !== 'Escape') return;

        // Close context menu first
        if (contextMenu.classList.contains('visible')) {
          contextMenu.classList.remove('visible');
          e.preventDefault();
          return;
        }

        // Find the topmost visible modal
        const modals = document.querySelectorAll('.modal-overlay.visible');
        if (modals.length === 0) return;
        const modal = modals[modals.length - 1];
        e.preventDefault();

        switch (modal.id) {
          case 'modal-overwrite':
            invoke('respond_overwrite', { response: 'no' });
            releaseFocusTrap(modal);
            modal.classList.remove('visible');
            break;
          case 'modal-fallback':
            invoke('respond_fallback', { response: 'no' });
            releaseFocusTrap(modal);
            modal.classList.remove('visible');
            break;
          case 'modal-delete-confirm':
            $('#dc-no').click();
            break;
          case 'modal-preflight':
            $('#pf-cancel').click();
            break;
          case 'modal-countdown':
            $('#cd-cancel').click();
            break;
          case 'modal-ffmpeg-missing':
            $('#ffmpeg-dl-no').click();
            break;
          case 'modal-unknown-codec':
            $('#uc-cancel').click();
            break;
          default:
            releaseFocusTrap(modal);
            modal.classList.remove('visible');
        }
      });
    }

    // ── Delete-source confirmation (returns promise) ──
    function showDeleteConfirm() {
      return new Promise((resolve) => {
        const modal = $('#modal-delete-confirm');
        modal.classList.add('visible');
        trapFocusInModal(modal);
        const yesBtn = $('#dc-yes');
        const noBtn = $('#dc-no');
        function cleanup() {
          releaseFocusTrap(modal);
          modal.classList.remove('visible');
          yesBtn.removeEventListener('click', onYes);
          noBtn.removeEventListener('click', onNo);
        }
        function onYes() { cleanup(); resolve(true); }
        function onNo()  { cleanup(); resolve(false); }
        yesBtn.addEventListener('click', onYes);
        noBtn.addEventListener('click', onNo);
      });
    }

    function showPreflightModal(warnings) {
      return new Promise((resolve) => {
        const modal = $('#modal-preflight');
        const tbody = $('#preflight-table-body');
        tbody.innerHTML = '';
        const hasMissing = warnings.some(w => w.missingTool);
        $('#modal-preflight-title').textContent = `${hasMissing ? 'Missing tools' : 'Files to check'}: ${warnings.length} ${warnings.length === 1 ? 'file' : 'files'}`;

        for (const w of warnings) {
          const tr = document.createElement('tr');
          for (const text of [w.fileName, w.sourceType, w.actualOutcome, w.missingTool || '-']) {
            const td = document.createElement('td');
            td.textContent = text;
            tr.appendChild(td);
          }
          tbody.appendChild(tr);
        }

        const dlBtn = $('#pf-download');
        dlBtn.style.display = hasMissing ? '' : 'none';

        modal.classList.add('visible');
        trapFocusInModal(modal);
        function cleanup() {
          releaseFocusTrap(modal);
          modal.classList.remove('visible');
          dlBtn.removeEventListener('click', onDl);
          $('#pf-continue').removeEventListener('click', onCont);
          $('#pf-cancel').removeEventListener('click', onCancel);
        }
        function onDl()     { cleanup(); resolve('download'); }
        function onCont()   { cleanup(); resolve('continue'); }
        function onCancel() { cleanup(); resolve('cancel'); }
        dlBtn.addEventListener('click', onDl);
        $('#pf-continue').addEventListener('click', onCont);
        $('#pf-cancel').addEventListener('click', onCancel);
      });
    }

    // ── Post-batch countdown (§7.6) ──
    let countdownInterval = null;

    function startCountdown(action, seconds) {
      if (seconds <= 0) {
        executePostAction(action);
        return;
      }
      let remaining = seconds;
      const modal = $('#modal-countdown');
      const body = $('#countdown-body');
      const valueEl = $('#countdown-value');
      body.textContent = '';
      const s1 = document.createElement('strong');
      s1.textContent = action;
      const s2 = document.createElement('strong');
      s2.id = 'countdown-value';
      s2.textContent = remaining;
      body.append(s1, ' in ', s2, ' seconds');
      modal.classList.add('visible');
      trapFocusInModal(modal);

      countdownInterval = setInterval(() => {
        remaining--;
        const v = modal.querySelector('#countdown-value');
        if (v) v.textContent = remaining;
        if (remaining <= 0) {
          clearCountdownTimer();
          modal.classList.remove('visible');
          executePostAction(action);
        }
      }, 1000);
    }

    function clearCountdownTimer() {
      if (countdownInterval) {
        clearInterval(countdownInterval);
        countdownInterval = null;
      }
    }

    async function executePostAction(action) {
      try {
        if (action === 'Custom Command') {
          // Custom command is handled differently - just log it for now
          appendLog(`[post-batch] Executing custom command: ${txtCustomCmd.value}`);
          // The backend could handle this, but for custom we'd invoke a separate command
        } else {
          await invoke('execute_post_batch_action', { action });
        }
      } catch (e) {
        appendLog(`[post-batch] Error: ${e}`);
      }
    }

    // ── Format seconds as HH:MM:SS ──
    function formatDuration(secs) {
      const h = Math.floor(secs / 3600);
      const m = Math.floor((secs % 3600) / 60);
      const s = Math.floor(secs % 60);
      if (h > 0) return `${h}:${String(m).padStart(2,'0')}:${String(s).padStart(2,'0')}`;
      return `${m}:${String(s).padStart(2,'0')}`;
    }

    function formatEta(secs) {
      if (!isFinite(secs) || secs < 0) return '';
      const s = Math.round(secs);
      if (s < 60) return `${s}s`;
      const m = Math.floor(s / 60);
      const rs = s % 60;
      if (m < 60) return `${m}m ${String(rs).padStart(2,'0')}s`;
      const h = Math.floor(m / 60);
      const rm = m % 60;
      return `${h}h ${String(rm).padStart(2,'0')}m`;
    }

    // ═══════════════════════════════════════════════════════════════
    //  TAURI EVENT LISTENERS
    // ═══════════════════════════════════════════════════════════════

    // Window close protection during batch (§16.4)
    window.addEventListener('beforeunload', (e) => {
      if (batchRunning) {
        e.preventDefault();
        e.returnValue = '';
      }
    });

    // Resolves once the backend has registered every listener, so events
    // emitted afterwards (such as the log replay) are not missed.
    function listenTauriEvents() {
      const registrations = [];
      const on = (event, handler) => { registrations.push(listen(event, handler)); };

      // Encoder detection complete
      on('encoder-detection-done', async () => {
        encoderDetectionDone = true;
        try {
          const [ve] = await invoke('get_detected_encoders');
          videoEncoders = ve;
          updateEncoderSummary();
        } catch (e) {
          encoderSummary.textContent = 'Encoder detection failed';
        }
        if (chkPrecision.checked) applyPrecisionMode();
        updateRcModeVisibility();
        updateBatchButtons();
      });

      // ffmpeg not found - prompt to download
      on('ffmpeg-missing', () => {
        encoderSummary.textContent = 'ffmpeg not found';
        const modal = document.querySelector('#modal-ffmpeg-missing');
        const body = document.querySelector('#ffmpeg-missing-body');
        modal.classList.add('visible');
        trapFocusInModal(modal);

        document.querySelector('#ffmpeg-dl-yes').onclick = async () => {
          releaseFocusTrap(modal);
          modal.classList.remove('visible');
          encoderSummary.textContent = 'Downloading ffmpeg';
          try {
            await invoke('download_ffmpeg');
          } catch (e) {
            encoderSummary.textContent = 'Download failed: ' + e;
            body.textContent = 'Download failed: ' + e + ' (install ffmpeg, then restart)';
            modal.classList.add('visible');
            trapFocusInModal(modal);
          }
        };

        document.querySelector('#ffmpeg-dl-no').onclick = () => {
          releaseFocusTrap(modal);
          modal.classList.remove('visible');
          encoderSummary.textContent = 'ffmpeg not found (install ffmpeg, then restart)';
        };
      });

      // Download progress updates
      on('ffmpeg-download-progress', (event) => {
        encoderSummary.textContent = event.payload;
      });

      // Log messages
      on('log', (event) => {
        appendLog(event.payload);
      });

      // ffmpeg stderr lines
      on('ffmpeg-stderr', (event) => {
        appendLog(event.payload);
      });

      // Per-file progress (duration-based)
      on('file-progress', (event) => {
        const { percent, timeSecs, totalSecs, pass } = event.payload;
        progressBar.style.width = percent.toFixed(1) + '%';
        if (progressBarContainer) progressBarContainer.setAttribute('aria-valuenow', Math.round(percent));
        const elapsed = formatDuration(timeSecs);
        const total = formatDuration(totalSecs);
        const passLabel = pass ? ` - Pass ${pass.current}/${pass.total}` : '';

        // ETA: wall-clock elapsed vs media time progress
        let etaLabel = '';
        if (fileEncodeStart > 0 && timeSecs > 1) {
          const wallElapsed = (performance.now() - fileEncodeStart) / 1000;
          const speed = timeSecs / wallElapsed;
          if (speed > 0) {
            const remaining = (totalSecs - timeSecs) / speed;
            const formatted = formatEta(remaining);
            if (formatted) etaLabel = ` - ETA ${formatted}`;
          }
        }

        jobProgressText.textContent = `${elapsed} / ${total} (${percent.toFixed(0)}%)${passLabel}${etaLabel}`;

        // Title bar: show percentage, file count, and ETA
        if (batchTotalFiles > 0) {
          const overallPct = Math.round(((batchCurrentFile - 1) / batchTotalFiles) * 100 + (1 / batchTotalFiles) * percent);
          const etaShort = etaLabel ? etaLabel.replace(' - ', ' ') : '';
          document.title = `[${overallPct}%] ${batchCurrentFile}/${batchTotalFiles}${etaShort} - ${defaultTitle}`;
        }
        // Interpolate into overall batch bar
        if (batchTotalFiles > 0) {
          const completedPct = ((batchCurrentFile - 1) / batchTotalFiles) * 100;
          const filePortion = (1 / batchTotalFiles) * percent;
          batchProgressBar.style.width = (completedPct + filePortion).toFixed(1) + '%';
          if (batchProgressBarContainer) batchProgressBarContainer.setAttribute('aria-valuenow', Math.round(completedPct + filePortion));
        }
        // Advance green fill on the currently-encoding queue row (by cached index)
        if (currentEncodingIndex >= 0) {
          const encodingRow = queueBody.children[currentEncodingIndex];
          if (encodingRow) {
            currentEncodeProgress = percent.toFixed(1) + '%';
            encodingRow.style.setProperty('--encode-progress', currentEncodeProgress);
          }
        }
      });

      // Queue item updated (status change) - patch only the affected row
      // instead of walking the entire queue table. Also tracks the currently-
      // encoding row index so the file-progress listener can skip querySelector.
      on('queue-item-updated', (event) => {
        const [index, status] = event.payload;
        if (index >= 0 && index < queueData.length) {
          queueData[index].status = status;
        }
        const statusLower = (status || 'pending').toLowerCase();
        // Track the currently-encoding row for file-progress
        if (statusLower === 'encoding') {
          currentEncodingIndex = index;
        } else if (currentEncodingIndex === index) {
          currentEncodingIndex = -1;
        }
        // Patch the single row if it exists; fall back to full render if row count mismatches
        const tr = queueBody.children[index];
        if (tr && queueBody.children.length === queueData.length) {
          const wantClass = `status-${statusLower}` + (selectedRows.has(index) ? ' selected' : '');
          if (tr.className !== wantClass) {
            tr.className = wantClass;
            tr.style.removeProperty('--encode-progress');
          }
          const c8 = tr.children[8];
          const badge = c8 && c8.firstChild;
          if (badge) {
            const icon = STATUS_ICONS[statusLower] || '';
            badge.innerHTML = '';
            if (icon) {
              const iconSpan = document.createElement('span');
              iconSpan.className = 'status-icon';
              iconSpan.innerHTML = icon;
              badge.appendChild(iconSpan);
            }
            badge.appendChild(document.createTextNode(status || 'Pending'));
            tr.dataset.status = statusLower;
          }
        } else {
          renderQueue();
        }
        updateBatchButtons();
      });

      // Queue item probed - the backend emits the full post-probe row as
      // payload (#3c), so we patch queueData[index] in place and schedule
      // a debounced render instead of refetching the whole queue. This
      // keeps the probe storm on large queues (~15k files) off the IPC
      // critical path: no get_queue clone, no ~6.5 MB JSON round trip per
      // probe event. renderQueue() is already a differential update, so
      // the DOM cost is bounded to cells that actually changed.
      on('queue-item-probed', (event) => {
        const [index, item] = event.payload;
        if (index >= queueData.length) {
          // Index out of sync with frontend state — fall back to a full
          // refresh so we don't silently desync.
          scheduleQueueRefresh();
          return;
        }
        queueData[index] = item;
        scheduleQueueRender();
      });

      // Batch started
      on('batch-started', () => {
        batchRunning = true;
        btnPause.textContent = 'Pause';
        jobStatus.textContent = 'Encoding';
        announceStatus('Encoding started');
        document.querySelector('#bottom-panel')?.classList.add('is-encoding');
        updateBatchButtons();
      });

      // Batch progress (file count)
      on('batch-progress', (event) => {
        const { current, total } = event.payload;
        batchCurrentFile = current;
        batchTotalFiles = total;
        fileEncodeStart = performance.now();
        jobStatus.textContent = `Encoding ${current}/${total}`;
        announceStatus(`Encoding file ${current} of ${total}`);
        // Reset per-file progress bar when a new file starts
        progressBar.style.width = '0%';
        if (progressBarContainer) progressBarContainer.setAttribute('aria-valuenow', 0);
        jobProgressText.textContent = '0%';
        // Update overall batch progress bar (completed files only)
        const batchPct = total > 0 ? ((current - 1) / total) * 100 : 0;
        batchProgressBar.style.width = batchPct + '%';
        if (batchProgressBarContainer) batchProgressBarContainer.setAttribute('aria-valuenow', Math.round(batchPct));
      });

      // Batch status label (current file name)
      on('batch-status', (event) => {
        jobStatus.textContent = 'Encoding';
        jobFile.textContent = event.payload;
        jobFile.title = event.payload;
        announceStatus('Encoding ' + event.payload);
      });

      // Batch command label
      on('batch-command', (event) => {
        statusCmd.textContent = event.payload;
        statusCmd.title = event.payload;
      });

      // Wave progress (Phase 3)
      on('wave-progress', (event) => {
        const { wave, totalWaves, fileInWave, waveSize } = event.payload;
        if (totalWaves > 0) {
          jobStatus.textContent = `Wave ${wave}/${totalWaves} [${fileInWave}/${waveSize} files] - Encoding file ${batchCurrentFile} of ${batchTotalFiles}`;
        }
      });

      // Wave status (Phase 3)
      on('wave-status', (event) => {
        jobStatus.textContent = event.payload;
      });

      // Batch finished
      on('batch-finished', async (event) => {
        batchRunning = false;
        document.querySelector('#bottom-panel')?.classList.remove('is-encoding');
        currentEncodingIndex = -1;
        fileEncodeStart = 0;
        document.title = defaultTitle;
        btnPause.textContent = 'Pause';
        jobStatus.textContent = 'Ready';
        statusCmd.textContent = '';
        jobFile.textContent = '';
        jobProgressText.textContent = '';
		currentEncodeProgress = '';
        progressBar.style.width = '0%';
        batchProgressBar.style.width = '0%';
        if (progressBarContainer) progressBarContainer.setAttribute('aria-valuenow', 0);
        if (batchProgressBarContainer) batchProgressBarContainer.setAttribute('aria-valuenow', 0);
        announceStatus('Batch finished');
        updateBatchButtons();

        // Auto-clear completed items if enabled
        if (chkAutoClear.checked) {
          try {
            await invoke('clear_completed');
          } catch (e) {
            console.warn('Auto-clear failed:', e);
          }
        }

        // Re-fetch queue (statuses may not be synced yet - queue-sync-complete
        // fires after the backend writes final statuses to the shared queue).
        selectedRows.clear();
        await fetchQueue();
        renderQueue();
      });

      // Queue sync complete — the backend syncs queue statuses after
      // batch-finished fires. Re-fetch to pick up the authoritative state,
      // then auto-continue if new files were added during the batch.
      on('queue-sync-complete', async (event) => {
        await fetchQueue();
        renderQueue();

        // Auto-continue: if new files were added during the batch, start another.
        // Check batchWasCancelled BEFORE clearing it so a user cancel actually
        // prevents auto-continue. The payload is true when the backend
        // cancelled or stopped the batch (encoder failure, cancel at a prompt).
        const batchStopped = event && event.payload === true;
        if (!batchRunning && !batchWasCancelled && !batchStopped) {
          const hasPending = queueData.some(i => (i.status || '').toLowerCase() === 'pending');
          if (hasPending) {
            startBatch(true, true);
          }
        }
        batchWasCancelled = false;
      });

      // Overwrite prompt from backend
      on('overwrite-prompt', (event) => {
        const filename = event.payload;
        $('#overwrite-body').textContent = filename;
        const owModal = $('#modal-overwrite');
        owModal.classList.add('visible');
        trapFocusInModal(owModal);
      });

      // Fallback prompt from backend
      on('fallback-prompt', (event) => {
        const filename = event.payload;
        $('#fallback-body').textContent = filename;
        const fbModal = $('#modal-fallback');
        fbModal.classList.add('visible');
        trapFocusInModal(fbModal);
      });

      // Toast notification (§7.7)
      on('toast', async (event) => {
        try {
          // Tauri v2 notification plugin
          const permitted = await invoke('plugin:notification|is_permission_granted');
          if (!permitted) {
            await invoke('plugin:notification|request_permission');
          }
          await invoke('plugin:notification|notify', {
            title: 'Honey, I Shrunk The Vids',
            body: event.payload,
          });
        } catch (e) {
          console.warn('Notification error:', e);
        }
      });

      // Post-batch action (§7.6)
      on('post-batch', (event) => {
        const { action, countdown } = event.payload;
        startCountdown(action, countdown);
      });

      // allSettled: one failed registration must not abort start-up, but it
      // is logged so a missing listener is not silent.
      return Promise.allSettled(registrations).then((results) => {
        results.forEach((r) => {
          if (r.status === 'rejected') console.error('Event listener registration failed:', r.reason);
        });
      });
    }

