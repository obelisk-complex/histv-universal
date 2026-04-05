const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const {
  formatBytes,
  formatDuration,
  formatEta,
  computeTargetBitrateLabel,
  computeEstimatedSize,
  formatEstimatedSize,
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
    targetCodecFamily: 'hevc',
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
    const item = { videoBitrateMbps: 4.5, videoCodec: 'h264' };
    const label = computeTargetBitrateLabel(item, baseSettings);
    assert.match(label, /VBR/);
  });
});

// ── computeEstimatedSize ───────────────────────────────────────

describe('computeEstimatedSize', () => {
  const baseSettings = {
    target: 4,
    peakMultiplier: 1.5,
    targetCodecFamily: 'hevc',
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
    targetCodecFamily: 'hevc',
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
