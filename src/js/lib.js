// Pure utility functions extracted from app.js for testability.
// These are used by both app.js and lib.test.js.

function formatBytes(n) {
  if (!n || n <= 0) return '-';
  if (n >= 1000000000) return (n / 1000000000).toFixed(2) + ' GB';
  return (n / 1000000).toFixed(1) + ' MB';
}

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

function computeTargetBitrateLabel(item, settings) {
  const mbps = item.videoBitrateMbps;
  const sourceCodec = (item.videoCodec || '').toLowerCase();
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

function computeEstimatedSize(item, settings) {
  if (!item.sourceBytes || item.sourceBytes <= 0) return null;

  const mbps = item.videoBitrateMbps;
  const sourceCodec = (item.videoCodec || '').toLowerCase();
  const isNonCopyable = ['gif', 'apng', 'mjpeg', 'webp'].includes(sourceCodec);

  if (!mbps || mbps <= 0) {
    if (!item.videoCodec) return null;
    return { bytes: item.sourceBytes, approx: true };
  }

  const isSameCodec = sourceCodec === resolveFamilyForCodec(sourceCodec, settings);

  if (mbps > settings.target * 1.15 || (mbps > settings.target && !isSameCodec)) {
    const estimatedBytes = (settings.target * 1000000 * item.durationSecs) / 8;
    return { bytes: Math.round(estimatedBytes), approx: false };
  } else if (isNonCopyable) {
    return { bytes: item.sourceBytes, approx: true };
  } else if (mbps > 0) {
    return { bytes: item.sourceBytes, approx: false };
  }

  return null;
}

function formatEstimatedSize(item, settings) {
  const est = computeEstimatedSize(item, settings);
  if (!est) return '-';
  const formatted = formatBytes(est.bytes);
  return est.approx ? '~' + formatted : formatted;
}

// Codec family the backend encodes a single source to (mirrors the codec_family
// "auto" branch of resolve_file_settings in encoder.rs): compatibility mode
// gives H.264; an H.264 source stays H.264; HEVC stays HEVC; AV1 stays AV1
// only when Preserve AV1 is on; everything else is converted to HEVC.
function resolveFamilyForCodec(codec, opts) {
  if (opts.compat) return 'h264';
  const c = (codec || '').toLowerCase();
  if (c === 'h264') return 'h264';
  if (c === 'hevc' || c === 'h265') return 'hevc';
  if (opts.preserveAv1 && c === 'av1') return 'av1';
  return 'hevc';
}

// Codec families the backend will encode to for the files still to be
// processed. Returns the de-duplicated set in the order hevc, h264, av1;
// ['hevc'] for an empty queue and ['h264'] in compatibility mode.
function resolveEncoderFamilies(items, opts) {
  if (opts.compat) return ['h264'];
  const active = (items || []).filter(it =>
    ['pending', 'probing', 'encoding'].includes((it.status || '').toLowerCase()));
  if (active.length === 0) return ['hevc'];
  const found = new Set(active.map(it => resolveFamilyForCodec(it.videoCodec, opts)));
  return ['hevc', 'h264', 'av1'].filter(f => found.has(f));
}

module.exports = {
  formatBytes,
  formatDuration,
  formatEta,
  computeTargetBitrateLabel,
  computeEstimatedSize,
  formatEstimatedSize,
  resolveFamilyForCodec,
  resolveEncoderFamilies,
};
