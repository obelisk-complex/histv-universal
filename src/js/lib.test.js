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
  const names = ['deriveDoubletTokens', 'hex6', 'contrastRatio', 'isLightColor', 'hexToRgb', 'rgbToHex',
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
