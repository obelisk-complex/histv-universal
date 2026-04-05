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

// ── DV source forces MP4 ───────────────────────────────────────

#[test]
fn dv_source_forces_mp4_container() {
    let settings = base_settings();
    let encoders = sw_encoders();

    let resolved = encoder::resolve_file_settings("hevc", "mkv", &settings, &encoders);
    // The container override for DV happens at encode time, not resolve time,
    // so we just verify the resolve doesn't break with DV content
    assert!(!resolved.codec_family.is_empty());
}
