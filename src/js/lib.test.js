const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const {
  formatBytes,
  formatDuration,
  formatEta,
  computeTargetBitrateLabel,
  computeEstimatedSize,
  formatEstimatedSize,
  resolveEncoderFamilies,
  normaliseUiStyle,
  uiStyleOfProperty,
} = require('./lib.js');

// ── formatBytes ────────────────────────────────────────────────

describe('formatBytes', () => {
  it('returns dash for zero', () => {
    assert.equal(formatBytes(0), '-');
  });

  it('returns dash for negative', () => {
    assert.equal(formatBytes(-100), '-');
  });

  it('returns dash for null/undefined', () => {
    assert.equal(formatBytes(null), '-');
    assert.equal(formatBytes(undefined), '-');
  });

  it('formats MB range', () => {
    assert.equal(formatBytes(1500000), '1.5 MB');
    assert.equal(formatBytes(999999999), '1000.0 MB');
  });

  it('formats GB range', () => {
    assert.equal(formatBytes(1000000000), '1.00 GB');
    assert.equal(formatBytes(2500000000), '2.50 GB');
  });

  it('formats small values in MB', () => {
    assert.equal(formatBytes(500000), '0.5 MB');
  });
});

// ── formatDuration ─────────────────────────────────────────────

describe('formatDuration', () => {
  it('formats seconds only', () => {
    assert.equal(formatDuration(45), '0:45');
  });

  it('formats minutes and seconds', () => {
    assert.equal(formatDuration(125), '2:05');
  });

  it('formats hours', () => {
    assert.equal(formatDuration(3661), '1:01:01');
  });

  it('zero-pads correctly', () => {
    assert.equal(formatDuration(3600), '1:00:00');
    assert.equal(formatDuration(60), '1:00');
  });

  it('handles zero', () => {
    assert.equal(formatDuration(0), '0:00');
  });
});

// ── formatEta ──────────────────────────────────────────────────

describe('formatEta', () => {
  it('returns empty for negative', () => {
    assert.equal(formatEta(-1), '');
  });

  it('returns empty for Infinity', () => {
    assert.equal(formatEta(Infinity), '');
  });

  it('returns empty for NaN', () => {
    assert.equal(formatEta(NaN), '');
  });

  it('formats seconds', () => {
    assert.equal(formatEta(30), '30s');
    assert.equal(formatEta(59), '59s');
  });

  it('formats minutes', () => {
    assert.equal(formatEta(120), '2m 00s');
    assert.equal(formatEta(90), '1m 30s');
  });

  it('formats hours', () => {
    assert.equal(formatEta(3600), '1h 00m');
    assert.equal(formatEta(3661), '1h 01m');
  });
});

// ── computeTargetBitrateLabel ──────────────────────────────────

describe('computeTargetBitrateLabel', () => {
  const baseSettings = {
    target: 4,
    peakMultiplier: 1.5,
    compat: false,
    preserveAv1: false,
    rcMode: 'QP',
    qpI: 20,
    qpP: 22,
    crf: 20,
  };

  it('returns Copy for same codec below threshold', () => {
    const item = { videoBitrateMbps: 3.0, videoCodec: 'hevc' };
    assert.equal(computeTargetBitrateLabel(item, baseSettings), 'Copy');
  });

  it('returns VBR for above threshold', () => {
    const item = { videoBitrateMbps: 10.0, videoCodec: 'hevc' };
    const label = computeTargetBitrateLabel(item, baseSettings);
    assert.match(label, /VBR/);
    assert.match(label, /4\/6Mbps/);
  });

  it('returns CQP for zero bitrate in QP mode', () => {
    const item = { videoBitrateMbps: 0, videoCodec: 'hevc' };
    assert.equal(computeTargetBitrateLabel(item, baseSettings), 'CQP (20/22)');
  });

  it('returns CRF for zero bitrate in CRF mode', () => {
    const item = { videoBitrateMbps: 0, videoCodec: 'hevc' };
    const crfSettings = { ...baseSettings, rcMode: 'CRF' };
    assert.equal(computeTargetBitrateLabel(item, crfSettings), 'CRF 20');
  });

  it('non-copyable codec uses quality mode even below threshold', () => {
    const item = { videoBitrateMbps: 2.0, videoCodec: 'gif' };
    const label = computeTargetBitrateLabel(item, baseSettings);
    assert.equal(label, 'CQP (20/22)');
  });

  it('different codec family at threshold gets VBR', () => {
    const item = { videoBitrateMbps: 4.5, videoCodec: 'vp9' };
    const label = computeTargetBitrateLabel(item, baseSettings);
    assert.match(label, /VBR/);
  });

  it('H.264 source stays H.264, so a small overshoot is copied', () => {
    const item = { videoBitrateMbps: 4.5, videoCodec: 'h264' };
    assert.equal(computeTargetBitrateLabel(item, baseSettings), 'Copy');
  });

  it('H.264 source in compatibility mode is also copied within the band', () => {
    const item = { videoBitrateMbps: 4.5, videoCodec: 'h264' };
    assert.equal(computeTargetBitrateLabel(item, { ...baseSettings, compat: true }), 'Copy');
  });

  it('HEVC source in compatibility mode gets VBR (target is H.264)', () => {
    const item = { videoBitrateMbps: 4.5, videoCodec: 'hevc' };
    assert.match(computeTargetBitrateLabel(item, { ...baseSettings, compat: true }), /VBR/);
  });

  it('AV1 source with Preserve AV1 stays AV1, so a small overshoot is copied', () => {
    const item = { videoBitrateMbps: 4.5, videoCodec: 'av1' };
    assert.match(computeTargetBitrateLabel(item, baseSettings), /VBR/);
    assert.equal(computeTargetBitrateLabel(item, { ...baseSettings, preserveAv1: true }), 'Copy');
  });
});

// ── computeEstimatedSize ───────────────────────────────────────

describe('computeEstimatedSize', () => {
  const baseSettings = {
    target: 4,
    peakMultiplier: 1.5,
    compat: false,
    preserveAv1: false,
    rcMode: 'QP',
    qpI: 20,
    qpP: 22,
    crf: 20,
  };

  it('returns null for zero sourceBytes', () => {
    const item = { sourceBytes: 0, videoBitrateMbps: 10 };
    assert.equal(computeEstimatedSize(item, baseSettings), null);
  });

  it('returns copy size for below threshold', () => {
    const item = { sourceBytes: 50000000, videoBitrateMbps: 3.0, videoCodec: 'hevc', durationSecs: 60 };
    const est = computeEstimatedSize(item, baseSettings);
    assert.equal(est.bytes, 50000000);
    assert.equal(est.approx, false);
  });

  it('returns VBR estimate for above threshold', () => {
    const item = { sourceBytes: 200000000, videoBitrateMbps: 10.0, videoCodec: 'hevc', durationSecs: 100 };
    const est = computeEstimatedSize(item, baseSettings);
    // 4Mbps * 100s / 8 = 50MB
    assert.equal(est.bytes, 50000000);
    assert.equal(est.approx, false);
  });

  it('returns approximate for quality mode', () => {
    const item = { sourceBytes: 50000000, videoBitrateMbps: 0, videoCodec: 'hevc', durationSecs: 60 };
    const est = computeEstimatedSize(item, baseSettings);
    assert.equal(est.bytes, 50000000);
    assert.equal(est.approx, true);
  });
});

// ── formatEstimatedSize ────────────────────────────────────────

describe('formatEstimatedSize', () => {
  const baseSettings = {
    target: 4,
    peakMultiplier: 1.5,
    compat: false,
    preserveAv1: false,
    rcMode: 'QP',
    qpI: 20,
    qpP: 22,
    crf: 20,
  };

  it('returns dash for no estimate', () => {
    const item = { sourceBytes: 0 };
    assert.equal(formatEstimatedSize(item, baseSettings), '-');
  });

  it('formats exact estimate without tilde', () => {
    const item = { sourceBytes: 50000000, videoBitrateMbps: 3.0, videoCodec: 'hevc', durationSecs: 60 };
    const result = formatEstimatedSize(item, baseSettings);
    assert.ok(!result.startsWith('~'));
    assert.match(result, /MB/);
  });

  it('formats approximate estimate with tilde', () => {
    const item = { sourceBytes: 50000000, videoBitrateMbps: 0, videoCodec: 'hevc', durationSecs: 60 };
    const result = formatEstimatedSize(item, baseSettings);
    assert.ok(result.startsWith('~'));
  });
});

// ── Theme colour derivation (extracted from app.js) ─────────────

const fs = require('node:fs');
const path = require('node:path');

// app.js is a browser script, so pull the colour functions out of its source.
function loadColourFns() {
  const src = fs.readFileSync(path.join(__dirname, 'app.js'), 'utf8');
  const names = ['deriveDoubletTokens', 'deriveClassicTokens', 'hexToRgba', 'isValidCssColor',
    'hex6', 'contrastRatio', 'isLightColor', 'hexToRgb', 'rgbToHex',
    'blendHex', 'shiftBrightness', 'relLuminance', 'bestTextOn', 'srgbToLinear', 'linearToSrgb',
    'hexToOklch', 'oklchToLinear', 'oklchToHex', 'liftLightness'];
  let code = '';
  for (const n of names) {
    const i = src.indexOf(`function ${n}(`);
    assert.ok(i >= 0, `app.js is missing ${n}`);
    let depth = 0;
    let k = src.indexOf('{', src.indexOf(')', i));
    for (; k < src.length; k++) {
      if (src[k] === '{') depth++;
      else if (src[k] === '}' && --depth === 0) break;
    }
    code += src.slice(i, k + 1) + '\n';
  }
  return new Function(`${code}\nreturn { ${names.join(', ')} };`)();
}

describe('deriveDoubletTokens', () => {
  const fns = loadColourFns();
  const dir = path.join(__dirname, '..', '..', 'src-tauri', 'themes');
  const themes = fs.readdirSync(dir).filter(f => f.endsWith('.json'))
    .map(f => JSON.parse(fs.readFileSync(path.join(dir, f), 'utf8')));
  const hueGap = (a, b) => Math.abs(((a - b + 540) % 360) - 180);

  for (const theme of themes) {
    it(`${theme.name}: text tokens reach 4.5:1 and thread line 3:1`, () => {
      const t = fns.deriveDoubletTokens(theme.colors);
      const cr = fns.contrastRatio;
      for (const ground of ['dx-cloth', 'dx-surface-dim', 'dx-surface', 'dx-surface-bright']) {
        for (const k of ['dx-text', 'dx-text-muted', 'dx-success-text', 'dx-error-text', 'dx-warn-text']) {
          assert.ok(cr(t[k], t[ground]) >= 4.5, `${k} on ${ground}: ${cr(t[k], t[ground]).toFixed(2)}`);
        }
        assert.ok(cr(t['dx-thread-line'], t[ground]) >= 3, `thread-line on ${ground}`);
      }
    });
  }

  it('Jessica Dark thread line keeps the primary hue and chroma', () => {
    const jessica = themes.find(t => t.name === 'Jessica Dark');
    const t = fns.deriveDoubletTokens(jessica.colors);
    const [, seedC, seedH] = fns.hexToOklch(jessica.colors.primary);
    const [, lineC, lineH] = fns.hexToOklch(t['dx-thread-line']);
    assert.ok(hueGap(seedH, lineH) <= 3, `hue ${seedH.toFixed(1)} vs ${lineH.toFixed(1)}`);
    assert.ok(lineC >= 0.85 * seedC, `chroma ${lineC.toFixed(3)} vs ${seedC.toFixed(3)}`);
  });

  it('lightness shift keeps hue and never adds chroma', () => {
    const [, c0, h0] = fns.hexToOklch('#5B2EBC');
    const out = fns.liftLightness('#5B2EBC', ['#0E0D17'], 6, false);
    const [l1, c1, h1] = fns.hexToOklch(out);
    assert.ok(fns.contrastRatio(out, '#0E0D17') >= 6);
    assert.ok(hueGap(h0, h1) <= 3);
    assert.ok(l1 > 0.45 && c1 <= c0 * 1.02);
    assert.equal(fns.liftLightness('#ffffff', ['#000000'], 4.5, false), '#ffffff');
  });
});

// ── Interface style (Doublet / Classic) ────────────────────────

describe('normaliseUiStyle', () => {
  it('keeps the two known styles', () => {
    assert.equal(normaliseUiStyle('classic'), 'classic');
    assert.equal(normaliseUiStyle('doublet'), 'doublet');
  });

  it('reads anything else as doublet, like the Rust config', () => {
    for (const v of [undefined, null, '', 'Classic', ' classic', 'neon', 42, {}]) {
      assert.equal(normaliseUiStyle(v), 'doublet', String(v));
    }
  });
});

describe('uiStyleOfProperty', () => {
  it('gives Doublet the --dx-* tokens', () => {
    assert.equal(uiStyleOfProperty('--dx-cloth'), 'doublet');
    assert.equal(uiStyleOfProperty('--dx-thread-line'), 'doublet');
  });

  it('gives Classic the pre-Doublet palette and glass tokens', () => {
    for (const p of ['--background', '--surface', '--text', '--primary', '--success', '--error',
      '--surface-dim', '--surface-bright', '--text-muted', '--accent', '--text-on-primary',
      '--row-info', '--row-encoding', '--progress-fill', '--progress-bg', '--icon-cancelled',
      '--glass-bg', '--glass-overlay-bg']) {
      assert.equal(uiStyleOfProperty(p), 'classic', p);
    }
  });

  it('leaves names both styles accept unowned', () => {
    for (const p of ['--log-warn', '--log-cmd', '--rail-w', '--encode-progress', '--base-100', 'color']) {
      assert.equal(uiStyleOfProperty(p), null, p);
    }
  });
});

describe('deriveClassicTokens', () => {
  const fns = loadColourFns();
  const dir = path.join(__dirname, '..', '..', 'src-tauri', 'themes');
  const themes = fs.readdirSync(dir).filter(f => f.endsWith('.json'))
    .map(f => JSON.parse(fs.readFileSync(path.join(dir, f), 'utf8')));

  // What applyTheme set inline at e4d0ee2 (2.6.0) for these themes.
  const pre3 = {
    'Classic Dark': {
      'background': '#1E1E1E', 'surface': '#373737', 'text': '#DCDCDC', 'primary': '#0078D7',
      'success': '#4ade80', 'error': '#f87171', 'surface-dim': '#2b2b2b',
      'surface-bright': '#4b4b4b', 'text-muted': 'rgba(220, 220, 220, 0.65)', 'accent': '#0078D7',
      'row-info': '#1a2c3a', 'row-done': '#253b2d', 'row-encoding': '#414141',
      'row-failed': '#3f2a2a', 'row-cancelled': '#403636', 'progress-fill': '#253b2d',
      'progress-bg': '#414141', 'icon-done': '#4ade80', 'icon-failed': '#f87171',
      'icon-cancelled': '#fb923c', 'log-result': '#4ade80', 'log-warn': '#fb923c',
      'log-error': '#f87171', 'log-detect': '#a78bfa', 'log-file': '#7dd3fc',
      'log-text': '#8BC88B', 'log-cmd': '#7a8090', 'text-on-primary': '#000000',
      'glass-bg': 'rgba(40, 40, 40, 0.65)', 'glass-border': 'rgba(255, 255, 255, 0.06)',
      'glass-input-bg': 'rgba(55, 55, 55, 0.5)', 'glass-th-bg': 'rgba(55, 55, 55, 0.9)',
      'glass-hover': 'rgba(255, 255, 255, 0.08)', 'glass-toggle-bg': 'rgba(55, 55, 55, 0.4)',
      'glass-log-bg': 'rgba(8, 8, 8, 0.9)', 'glass-strip-bg': 'rgba(40, 40, 40, 0.7)',
      'glass-overlay-bg': 'rgba(40, 40, 40, 0.92)',
    },
    'Classic Light': {
      'background': '#FFFFFF', 'surface': '#E0E0E0', 'text': '#1E1E1E', 'primary': '#0078D7',
      'success': '#22c55e', 'error': '#ef4444', 'surface-dim': '#f0f0f0',
      'surface-bright': '#cccccc', 'text-muted': 'rgba(30, 30, 30, 0.65)', 'accent': '#0078D7',
      'row-info': '#d9ebf9', 'row-done': '#e4f8ec', 'row-encoding': '#d6d6d6',
      'row-failed': '#fde9e9', 'row-cancelled': '#eae0e0', 'progress-fill': '#e4f8ec',
      'progress-bg': '#d6d6d6', 'icon-done': '#22c55e', 'icon-failed': '#ef4444',
      'icon-cancelled': '#fb923c', 'log-result': '#22c55e', 'log-warn': '#fb923c',
      'log-error': '#ef4444', 'log-detect': '#a78bfa', 'log-file': '#7dd3fc',
      'log-text': '#8BC88B', 'log-cmd': '#6b7280', 'text-on-primary': '#000000',
      'glass-bg': 'rgba(255, 255, 255, 0.55)', 'glass-border': 'rgba(0, 0, 0, 0.08)',
      'glass-input-bg': 'rgba(0, 0, 0, 0.04)', 'glass-th-bg': 'rgba(255, 255, 255, 0.8)',
      'glass-hover': 'rgba(0, 0, 0, 0.04)', 'glass-toggle-bg': 'rgba(0, 0, 0, 0.04)',
      'glass-log-bg': 'rgba(20, 20, 20, 0.95)', 'glass-strip-bg': 'rgba(255, 255, 255, 0.6)',
      'glass-overlay-bg': 'rgba(255, 255, 255, 0.92)',
    },
    'Creditor': {
      'background': '#26070F', 'surface': '#47101A', 'text': '#F5E9CF', 'primary': '#D9A21B',
      'success': '#4FD98A', 'error': '#E5483F', 'surface-dim': '#370c15',
      'surface-bright': '#342E30', 'text-muted': 'rgba(245, 233, 207, 0.65)', 'accent': '#D9A21B',
      'row-info': '#411e11', 'row-done': '#2c2721', 'row-encoding': '#511a24',
      'row-failed': '#431116', 'row-cancelled': '#4a161d', 'progress-fill': '#2c2721',
      'progress-bg': '#511a24', 'icon-done': '#4FD98A', 'icon-failed': '#E5483F',
      'icon-cancelled': '#E3B02A', 'log-result': '#4FD98A', 'log-warn': '#E3B02A',
      'log-error': '#E5483F', 'log-detect': '#a78bfa', 'log-file': '#7dd3fc',
      'log-text': '#8BC88B', 'log-cmd': '#7a8090', 'text-on-primary': '#000000',
      'glass-bg': 'rgba(40, 40, 40, 0.65)', 'glass-border': 'rgba(255, 255, 255, 0.06)',
      'glass-input-bg': 'rgba(55, 55, 55, 0.5)', 'glass-th-bg': 'rgba(55, 55, 55, 0.9)',
      'glass-hover': 'rgba(255, 255, 255, 0.08)', 'glass-toggle-bg': 'rgba(55, 55, 55, 0.4)',
      'glass-log-bg': 'rgba(8, 8, 8, 0.9)', 'glass-strip-bg': 'rgba(40, 40, 40, 0.7)',
      'glass-overlay-bg': 'rgba(40, 40, 40, 0.92)',
    },
  };

  for (const [name, expected] of Object.entries(pre3)) {
    it(`${name}: matches the 2.6.0 derivation`, () => {
      const theme = themes.find(t => t.name === name);
      assert.deepEqual(fns.deriveClassicTokens(theme.colors), expected);
    });
  }

  it('drops an invalid override instead of setting it', () => {
    const t = fns.deriveClassicTokens({ background: '#1E1E1E', 'row-info': 'red; x', accent: 'url(a)' });
    assert.equal(t['row-info'], undefined);
    assert.equal(t['accent'], undefined);
    assert.equal(t['background'], '#1E1E1E');
  });

  for (const theme of themes) {
    it(`${theme.name}: sets no Doublet token`, () => {
      for (const key of Object.keys(fns.deriveClassicTokens(theme.colors))) {
        assert.notEqual(uiStyleOfProperty(`--${key}`), 'doublet', key);
      }
    });
  }
});

describe('Doublet pairings under Classic', () => {
  // app.js gives each pairing its six seed colours so Classic can derive from
  // them; they must stay equal to the pairing blocks in doublet-tokens.css.
  const app = fs.readFileSync(path.join(__dirname, 'app.js'), 'utf8');
  const css = fs.readFileSync(path.join(__dirname, '..', 'css', 'doublet-tokens.css'), 'utf8');
  const literal = /const DOUBLET_THEMES = (\[[\s\S]*?\n\s*\]);/.exec(app);
  assert.ok(literal, 'DOUBLET_THEMES not found in app.js');
  const pairings = new Function(`return ${literal[1]};`)();
  const seedToken = { background: 'cloth', surface: 'surface', text: 'text', primary: 'thread', success: 'success', error: 'error' };

  for (const p of pairings) {
    it(`${p.name} seeds match doublet-tokens.css`, () => {
      const block = new RegExp(`/\\* ${p.pairing} \\*/\\s*:root[^{]*\\{([^}]*)\\}`).exec(css);
      assert.ok(block, `no ${p.pairing} block`);
      for (const [key, token] of Object.entries(seedToken)) {
        const m = new RegExp(`--dx-${token}:\\s*(#[0-9A-Fa-f]{6});`).exec(block[1]);
        assert.ok(m, `${p.pairing} has no --dx-${token}`);
        assert.equal(p.colors[key].toUpperCase(), m[1].toUpperCase(), `${p.pairing} ${key}`);
      }
    });
  }
});

describe('computeEstimatedSize target family', () => {
  const settings = {
    target: 4, compat: false, preserveAv1: false,
    rcMode: 'QP', qpI: 20, qpP: 22, crf: 20, peakMultiplier: 1.5,
  };
  const h264 = { sourceBytes: 90000000, videoBitrateMbps: 4.5, videoCodec: 'h264', durationSecs: 160 };

  it('uses the source size for an H.264 source within the copy band', () => {
    assert.deepEqual(computeEstimatedSize(h264, settings), { bytes: 90000000, approx: false });
  });

  it('estimates a re-encode for a non-H.264 source at the same bitrate', () => {
    const vp9 = { ...h264, videoCodec: 'vp9' };
    assert.deepEqual(computeEstimatedSize(vp9, settings), { bytes: 80000000, approx: false });
  });
});

// ── resolveEncoderFamilies ─────────────────────────────────────

describe('resolveEncoderFamilies', () => {
  const av1 = { videoCodec: 'av1', status: 'Pending' };
  const hevc = { videoCodec: 'hevc', status: 'Pending' };
  const vp9 = { videoCodec: 'vp9', status: 'Pending' };
  const h264 = { videoCodec: 'h264', status: 'Pending' };
  const on = { compat: false, preserveAv1: true };

  it('is HEVC for an empty queue even with Preserve AV1 ticked', () => {
    assert.deepEqual(resolveEncoderFamilies([], on), ['hevc']);
  });

  it('is HEVC when Preserve AV1 is ticked but no file is AV1', () => {
    assert.deepEqual(resolveEncoderFamilies([hevc, vp9], on), ['hevc']);
  });

  it('is AV1 when every file is AV1', () => {
    assert.deepEqual(resolveEncoderFamilies([av1, av1], on), ['av1']);
  });

  it('lists HEVC and AV1 for a mixed queue', () => {
    assert.deepEqual(resolveEncoderFamilies([av1, hevc], on), ['hevc', 'av1']);
  });

  it('treats unprobed files as non-AV1', () => {
    assert.deepEqual(resolveEncoderFamilies([av1, { status: 'Pending' }], on), ['hevc', 'av1']);
  });

  it('is HEVC for AV1 files when Preserve AV1 is off', () => {
    assert.deepEqual(resolveEncoderFamilies([av1], { compat: false, preserveAv1: false }), ['hevc']);
  });

  it('is H.264 in compatibility mode', () => {
    assert.deepEqual(resolveEncoderFamilies([av1], { compat: true, preserveAv1: true }), ['h264']);
  });

  it('keeps an H.264 source as H.264', () => {
    assert.deepEqual(resolveEncoderFamilies([h264], { compat: false, preserveAv1: false }), ['h264']);
    assert.deepEqual(resolveEncoderFamilies([h264], on), ['h264']);
  });

  it('lists HEVC and H.264 for H.264 plus another codec', () => {
    assert.deepEqual(resolveEncoderFamilies([h264, vp9], { compat: false, preserveAv1: false }), ['hevc', 'h264']);
    assert.deepEqual(resolveEncoderFamilies([vp9, h264], on), ['hevc', 'h264']);
  });

  it('lists H.264 and AV1 for H.264 plus AV1 with Preserve AV1 on', () => {
    assert.deepEqual(resolveEncoderFamilies([h264, av1], on), ['h264', 'av1']);
    assert.deepEqual(resolveEncoderFamilies([av1, h264], on), ['h264', 'av1']);
  });

  it('lists H.264 and HEVC for H.264 plus AV1 with Preserve AV1 off', () => {
    assert.deepEqual(resolveEncoderFamilies([h264, av1], { compat: false, preserveAv1: false }), ['hevc', 'h264']);
  });

  it('orders a three-way mix as HEVC, H.264, AV1', () => {
    assert.deepEqual(resolveEncoderFamilies([av1, h264, hevc], on), ['hevc', 'h264', 'av1']);
  });

  it('treats h265 as HEVC', () => {
    assert.deepEqual(resolveEncoderFamilies([{ videoCodec: 'h265', status: 'Pending' }], on), ['hevc']);
  });

  it('skips finished H.264 files', () => {
    assert.deepEqual(resolveEncoderFamilies([{ videoCodec: 'h264', status: 'Done' }, hevc], on), ['hevc']);
  });

  it('ignores files that are already finished', () => {
    const done = { videoCodec: 'av1', status: 'Done' };
    assert.deepEqual(resolveEncoderFamilies([done, hevc], on), ['hevc']);
    assert.deepEqual(resolveEncoderFamilies([done], on), ['hevc']);
  });

  it('compatibility mode gives H.264 whatever the queue holds', () => {
    const compat = { compat: true, preserveAv1: false };
    assert.deepEqual(resolveEncoderFamilies([], compat), ['h264']);
    assert.deepEqual(resolveEncoderFamilies([{ videoCodec: 'hevc', status: 'Done' }], compat), ['h264']);
    assert.deepEqual(resolveEncoderFamilies([hevc, av1], compat), ['h264']);
  });
});

// ── app.js keeps the same rules as lib.js ──────────────────────
//
// index.html loads only app.js, so app.js carries inline copies of the
// family helpers that lib.js exports for testing. Nothing else notices if
// the two drift apart, so compare the function text here.

describe('app.js inline copies match lib.js', () => {
  const fs = require('node:fs');
  const path = require('node:path');
  const lib = fs.readFileSync(path.join(__dirname, 'lib.js'), 'utf8');
  const app = fs.readFileSync(path.join(__dirname, 'app.js'), 'utf8');

  // Function source from `function name(` to its closing brace at the same
  // indent, with each line trimmed.
  function extract(src, name) {
    const m = new RegExp(`^( *)function ${name}\\(`, 'm').exec(src);
    assert.ok(m, `${name} not found`);
    const end = new RegExp(`^${m[1]}}$`, 'm').exec(src.slice(m.index));
    assert.ok(end, `${name} has no closing brace`);
    return src.slice(m.index, m.index + end.index + end[0].length)
      .split('\n').map((l) => l.trim()).filter(Boolean).join('\n');
  }

  it('resolveFamilyForCodec is identical', () => {
    assert.equal(extract(app, 'resolveFamilyForCodec'), extract(lib, 'resolveFamilyForCodec'));
  });

  it('normaliseUiStyle is identical', () => {
    assert.equal(extract(app, 'normaliseUiStyle'), extract(lib, 'normaliseUiStyle'));
  });

  it('uiStyleOfProperty is identical', () => {
    assert.equal(extract(app, 'uiStyleOfProperty'), extract(lib, 'uiStyleOfProperty'));
  });

  it('resolveEncoderFamilies keeps the same rules', () => {
    // app.js reads the page state, so only the shared rule lines are compared.
    const shared = [
      "if (opts.compat) return ['h264'];",
      "if (active.length === 0) return ['hevc'];",
      "['pending', 'probing', 'encoding'].includes((it.status || '').toLowerCase()));",
      "const found = new Set(active.map(it => resolveFamilyForCodec(it.videoCodec, opts)));",
      "return ['hevc', 'h264', 'av1'].filter(f => found.has(f));",
    ];
    const a = extract(app, 'resolveEncoderFamilies');
    const l = extract(lib, 'resolveEncoderFamilies');
    for (const line of shared) {
      assert.ok(a.includes(line), `app.js is missing: ${line}`);
      assert.ok(l.includes(line), `lib.js is missing: ${line}`);
    }
  });
});
