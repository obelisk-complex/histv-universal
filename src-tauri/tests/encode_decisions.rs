//! Integration tests for the encode decision pipeline.
//!
//! These exercise the full path from a ProbeResult through
//! resolve_file_settings() and decide_encode_strategy() without
//! needing ffmpeg installed.

use histv_lib::encoder::{self, BatchSettings, EncodeDecision, EncoderInfo, RateControlParams};

fn sw_encoders() -> Vec<EncoderInfo> {
    vec![
        EncoderInfo {
            name: "libx265".to_string(),
            codec_family: "hevc".to_string(),
            is_hardware: false,
        },
        EncoderInfo {
            name: "libx264".to_string(),
            codec_family: "h264".to_string(),
            is_hardware: false,
        },
    ]
}

fn base_settings() -> BatchSettings {
    BatchSettings {
        compatibility_mode: false,
        preserve_av1: false,
        precision_mode: false,
        output_folder: String::new(),
        output_mode: String::new(),
        threshold: 4.0,
        qp_i: 20,
        qp_p: 22,
        crf_val: 20,
        rate_control_mode: "QP".to_string(),
        pix_fmt: String::new(),
        delete_source: false,
        save_log: false,
        post_command: None,
        peak_multiplier: 1.5,
        threads: 0,
        low_priority: false,
        force_local: false,
        video_encoder: "auto".to_string(),
        codec_family: "auto".to_string(),
        audio_encoder: "auto".to_string(),
        audio_cap: 640,
        output_container: "auto".to_string(),
    }
}

fn rc_qp() -> RateControlParams<'static> {
    RateControlParams {
        mode: "QP",
        qp_i: 20,
        qp_p: 22,
        crf_val: 20,
    }
}

fn rc_crf() -> RateControlParams<'static> {
    RateControlParams {
        mode: "CRF",
        qp_i: 20,
        qp_p: 22,
        crf_val: 20,
    }
}

// ── Copy decisions ──────────────────────────────────────────────

#[test]
fn hevc_below_threshold_is_copy() {
    let decision = encoder::decide_encode_strategy(
        2.0, // source bitrate below 4.0 threshold
        4.0, // threshold
        "hevc",
        "hevc",
        &rc_qp(),
        1.5,
    );
    assert!(
        matches!(decision, EncodeDecision::Copy),
        "HEVC below threshold should be Copy, got: {:?}",
        decision
    );
}

#[test]
fn hevc_at_threshold_is_copy() {
    let decision = encoder::decide_encode_strategy(
        4.0, // at threshold
        4.0,
        "hevc",
        "hevc",
        &rc_qp(),
        1.5,
    );
    assert!(
        matches!(decision, EncodeDecision::Copy),
        "HEVC at threshold should be Copy, got: {:?}",
        decision
    );
}

// ── VBR decisions ──────────��────────────────────────────────────

#[test]
fn hevc_above_threshold_is_vbr() {
    let decision = encoder::decide_encode_strategy(10.0, 4.0, "hevc", "hevc", &rc_qp(), 1.5);
    assert!(
        matches!(decision, EncodeDecision::Vbr { .. }),
        "HEVC above threshold should be VBR, got: {:?}",
        decision
    );
}

#[test]
fn h264_above_threshold_is_vbr() {
    let decision = encoder::decide_encode_strategy(10.0, 4.0, "h264", "hevc", &rc_qp(), 1.5);
    assert!(
        matches!(decision, EncodeDecision::Vbr { .. }),
        "H264 above threshold should be VBR, got: {:?}",
        decision
    );
}

#[test]
fn vbr_peak_uses_multiplier() {
    let decision = encoder::decide_encode_strategy(10.0, 4.0, "hevc", "hevc", &rc_qp(), 2.0);
    if let EncodeDecision::Vbr {
        target_bps,
        peak_bps,
    } = decision
    {
        let expected_peak = target_bps * 2;
        assert!(
            (peak_bps as i64 - expected_peak as i64).unsigned_abs() < 100,
            "Peak should be 2x target: target={target_bps}, peak={peak_bps}"
        );
    } else {
        panic!("Expected VBR, got: {:?}", decision);
    }
}

// ── CQP/CRF decisions ──────────────────────────────────────────

#[test]
fn zero_bitrate_source_uses_quality_mode() {
    let decision = encoder::decide_encode_strategy(
        0.0, // unknown bitrate
        4.0,
        "hevc",
        "hevc",
        &rc_qp(),
        1.5,
    );
    assert!(
        matches!(decision, EncodeDecision::Cqp { .. }),
        "Zero-bitrate source with QP mode should be CQP, got: {:?}",
        decision
    );
}

#[test]
fn zero_bitrate_crf_mode() {
    let decision = encoder::decide_encode_strategy(0.0, 4.0, "hevc", "hevc", &rc_crf(), 1.5);
    assert!(
        matches!(decision, EncodeDecision::Crf { .. }),
        "Zero-bitrate source with CRF mode should be CRF, got: {:?}",
        decision
    );
}

// ── Codec family conversion ───���─────────────────────────────────

#[test]
fn different_codec_above_threshold_encodes() {
    // H264 source targeting HEVC above threshold - should encode
    let decision = encoder::decide_encode_strategy(
        10.0, // above threshold
        4.0,
        "h264",
        "hevc",
        &rc_qp(),
        1.5,
    );
    assert!(
        matches!(decision, EncodeDecision::Vbr { .. }),
        "H264->HEVC above threshold should be VBR, got: {:?}",
        decision
    );
}

// ── Compatibility mode ───��─────────────────────────────────���────

#[test]
fn compat_mode_forces_h264() {
    let mut settings = base_settings();
    settings.compatibility_mode = true;
    let encoders = sw_encoders();

    let resolved = encoder::resolve_file_settings("hevc", "mkv", &settings, &encoders);
    assert_eq!(resolved.codec_family, "h264");
    assert!(matches!(
        resolved.audio_strategy,
        encoder::AudioStrategy::CompatCapped { .. }
    ));
    assert_eq!(resolved.container_ext, "mp4");
}

// ── preserve_av1 ────────────���───────────────────────────────────

#[test]
fn preserve_av1_keeps_av1_codec() {
    let mut settings = base_settings();
    settings.preserve_av1 = true;
    let encoders = sw_encoders();

    let resolved = encoder::resolve_file_settings("av1", "mkv", &settings, &encoders);
    assert_eq!(resolved.codec_family, "av1");
}

// ── Image source always encodes ─────────────────────────────────

#[test]
fn gif_always_encodes() {
    let decision = encoder::decide_encode_strategy(0.0, 4.0, "gif", "hevc", &rc_qp(), 1.5);
    assert!(
        !matches!(decision, EncodeDecision::Copy),
        "GIF should never be Copy, got: {:?}",
        decision
    );
}

// ── Precision mode ──────────��───────────────────────────────────

#[test]
fn precision_mode_forces_software() {
    let mut settings = base_settings();
    settings.precision_mode = true;
    let mut encoders = sw_encoders();
    encoders.insert(
        0,
        EncoderInfo {
            name: "hevc_nvenc".to_string(),
            codec_family: "hevc".to_string(),
            is_hardware: true,
        },
    );

    let resolved = encoder::resolve_file_settings("hevc", "mkv", &settings, &encoders);
    // Precision mode should prefer software encoder
    assert!(
        !resolved.encoder_name.contains("nvenc"),
        "Precision mode should not use hardware encoder, got: {}",
        resolved.encoder_name
    );
}

// ── DV source: container derivation ────────────────────────────

// DV content is always MP4-wrapped in practice, so resolve_file_settings
// with a DV source that is already MP4 should keep MP4.  The encode-time
// override via resolve_container(is_dovi_tier1=true) is a separate path
// that handles the rare case of DV packaged in a non-MP4 container.
#[test]
fn dv_mp4_source_produces_mp4_container() {
    let settings = base_settings();
    let encoders = sw_encoders();

    // DV sources are MP4 in practice; auto mode should derive mp4 from the
    // source extension, matching what the wave cleanup now does via
    // resolve_file_settings.
    let resolved = encoder::resolve_file_settings("hevc", "mp4", &settings, &encoders);
    assert_eq!(
        resolved.container_ext, "mp4",
        "DV source in MP4 container should resolve to mp4"
    );
}

// ── Wave cleanup extension fix: compat, explicit container, auto ──
//
// These tests validate the three cases that were broken before the fix
// (resolve_container was called instead of resolve_file_settings):
//
// 1. Compat mode forces MP4 regardless of source extension or --container.
// 2. Explicit --container override (mp4/mkv) is respected.
// 3. Auto mode derives the container from the source extension.
//
// The wave cleanup now calls resolve_file_settings for all three, so
// these assertions confirm that function produces the right answer.

/// Compat mode must produce mp4 for any source extension, even when the
/// user set --container mkv.  Before the fix, resolve_container was called
/// with is_dovi_tier1=false and no compat awareness, so it would honour
/// --container mkv and produce mkv -- silently mismatching the encoder's
/// output.
#[test]
fn wave_cleanup_compat_mode_forces_mp4_over_mkv_container() {
    let mut settings = base_settings();
    settings.compatibility_mode = true;
    settings.output_container = "mkv".to_string(); // explicit override that compat must win over
    let encoders = sw_encoders();

    for source_ext in &["mkv", "avi", "mp4", "ts"] {
        let resolved = encoder::resolve_file_settings("hevc", source_ext, &settings, &encoders);
        assert_eq!(
            resolved.container_ext, "mp4",
            "compat mode must force mp4 for source_ext={source_ext} even with --container mkv"
        );
    }
}

/// Explicit --container mp4 must be respected when compat mode is off.
/// The old code also passed --container through resolve_container, so this
/// case happened to work - but we verify it here to guard against regression.
#[test]
fn wave_cleanup_explicit_mp4_container_respected() {
    let mut settings = base_settings();
    settings.output_container = "mp4".to_string();
    let encoders = sw_encoders();

    let resolved = encoder::resolve_file_settings("hevc", "mkv", &settings, &encoders);
    assert_eq!(
        resolved.container_ext, "mp4",
        "explicit --container mp4 should produce mp4"
    );
}

/// Explicit --container mkv must be respected when compat mode is off.
#[test]
fn wave_cleanup_explicit_mkv_container_respected() {
    let mut settings = base_settings();
    settings.output_container = "mkv".to_string();
    let encoders = sw_encoders();

    // Source is mp4 but --container mkv overrides the auto derivation.
    let resolved = encoder::resolve_file_settings("hevc", "mp4", &settings, &encoders);
    assert_eq!(
        resolved.container_ext, "mkv",
        "explicit --container mkv should produce mkv even for an mp4 source"
    );
}

/// Auto mode (output_container = "auto") derives the container from the
/// source file extension.  Before the fix the wave cleanup used
/// resolve_container with the original remote path, which would strip the
/// staging prefix correctly but ignore compat mode.  With the fix it calls
/// resolve_file_settings using only the extension, which should give the
/// same auto-derivation result for the normal (non-compat) case.
#[test]
fn wave_cleanup_auto_container_derives_from_source_ext() {
    let settings = base_settings(); // output_container = "auto"
    let encoders = sw_encoders();

    let mp4 = encoder::resolve_file_settings("hevc", "mp4", &settings, &encoders);
    assert_eq!(mp4.container_ext, "mp4", "auto + mp4 source → mp4");

    let mkv = encoder::resolve_file_settings("hevc", "mkv", &settings, &encoders);
    assert_eq!(mkv.container_ext, "mkv", "auto + mkv source → mkv");

    let avi = encoder::resolve_file_settings("hevc", "avi", &settings, &encoders);
    assert_eq!(
        avi.container_ext, "mkv",
        "auto + avi source → mkv (default)"
    );

    let ts = encoder::resolve_file_settings("hevc", "ts", &settings, &encoders);
    assert_eq!(ts.container_ext, "mkv", "auto + ts source → mkv (default)");
}

// ── resolve_container unit tests ───────────────────────────────

/// resolve_container with is_dovi_tier1=true always forces mp4, regardless
/// of the container_setting or source extension.
#[test]
fn resolve_container_dovi_tier1_always_forces_mp4() {
    for container in &["auto", "mkv", "mp4"] {
        let result = encoder::resolve_container("video.mkv", container, true);
        assert_eq!(
            result, "mp4",
            "DV Tier 1 must force mp4 with container={container}"
        );
    }
}

/// resolve_container without DV, explicit settings.
#[test]
fn resolve_container_explicit_settings_respected() {
    assert_eq!(encoder::resolve_container("video.mkv", "mp4", false), "mp4");
    assert_eq!(encoder::resolve_container("video.mp4", "mkv", false), "mkv");
}

/// resolve_container auto mode derives from source extension.
#[test]
fn resolve_container_auto_derives_from_source() {
    assert_eq!(
        encoder::resolve_container("video.mp4", "auto", false),
        "mp4"
    );
    assert_eq!(
        encoder::resolve_container("video.mkv", "auto", false),
        "mkv"
    );
    assert_eq!(
        encoder::resolve_container("video.avi", "auto", false),
        "mkv"
    );
    assert_eq!(encoder::resolve_container("video.ts", "auto", false), "mkv");
}
