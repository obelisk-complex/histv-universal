//! Dolby Vision RPU extract/inject pipeline.
//!
//! Tier 1 (full DV preservation) flow:
//! 1. Pre-encode: demux source to raw HEVC, extract RPU NALUs via `dolby_vision` crate
//! 2. Normal ffmpeg encode (unchanged)
//! 3. Post-encode: demux encoded output to raw HEVC, inject RPUs back, mux with MP4Box
//!
//! All bitstream processing uses streaming I/O via `hevc_utils::NalReader`
//! to avoid loading multi-gigabyte files into memory.

#[cfg(feature = "dovi")]
use dolby_vision::rpu::dovi_rpu::DoviRpu;
#[cfg(feature = "dovi")]
use dolby_vision::rpu::ConversionMode;

use std::path::{Path, PathBuf};
use tempfile::TempDir;

use crate::events::EventSink;
use crate::ffmpeg as ffbin;
use crate::hevc_utils;

/// Extracted RPU data from a source file, ready for injection after re-encode.
pub struct ExtractedRpus {
    /// One RPU per frame, in decode order. Only the RPU payload is stored
    /// (a few hundred bytes each), not the full video bitstream.
    pub rpus: Vec<Vec<u8>>,
    /// DV profile of the source.
    pub source_profile: u8,
    /// Whether profile conversion to 8.1 was applied.
    pub converted_to_81: bool,
}

/// Result of the full DV pipeline (inject + MP4Box packaging).
pub struct DoviPipelineResult {
    pub output_path: PathBuf,
    pub success: bool,
    pub message: String,
}

// ── RPU Extraction ────────────────────────────────────────────────

/// Extract RPU data from a source video file.
///
/// Demuxes the HEVC bitstream via ffmpeg, then streams through NAL units
/// to collect RPU data. Only RPU payloads are held in memory (typically
/// a few hundred bytes per frame), not the full bitstream.
#[cfg(feature = "dovi")]
pub async fn extract_rpus(
    source_path: &Path,
    dovi_profile: u8,
    dovi_bl_compat_id: Option<u8>,
    sink: &dyn EventSink,
) -> Result<ExtractedRpus, String> {
    let temp_dir = make_tempdir("histv_dovi_", source_path.parent())?;
    let raw_hevc = temp_dir.path().join("source.h265");

    sink.log("  DV: Extracting HEVC bitstream from source...");
    let demux_status = ffbin::ffmpeg_command()
        .args([
            "-y",
            "-i",
            &source_path.to_string_lossy(),
            "-c:v",
            "copy",
            "-bsf:v",
            "hevc_mp4toannexb",
            "-an",
            "-sn",
            "-f",
            "hevc",
            &raw_hevc.to_string_lossy(),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map_err(|e| format!("Failed to launch ffmpeg for demux: {e}"))?;

    if !demux_status.success() {
        return Err("ffmpeg demux to raw HEVC failed".to_string());
    }

    // Stream through the bitstream, collecting only RPU NAL units
    sink.log("  DV: Parsing RPU data from bitstream (streaming)...");
    let nalu_data = hevc_utils::extract_nalus_filtered(&raw_hevc, |nalu| {
        nalu.nal_type == hevc_utils::HEVC_NAL_UNSPEC62
    })
    .map_err(|e| format!("Failed to read bitstream: {e}"))?;

    // TempDir dropped here — large demuxed file cleaned up automatically
    drop(temp_dir);

    if nalu_data.is_empty() {
        return Err("No DV RPU NAL units found in bitstream".to_string());
    }

    // Convert profiles without an HDR10 fallback layer to Profile 8.1.
    // Profile 5: single-layer DV, no HDR10 base — non-DV players get nothing.
    // Profile 7 (compat_id=0): dual-layer DV, no HDR10 fallback in base.
    // Converting to 8.1 gives both DV and HDR10 compatibility in the output.
    let needs_conversion = dovi_profile == 5
        || (dovi_profile == 7 && dovi_bl_compat_id.map(|id| id == 0).unwrap_or(true));

    let mut rpu_payloads: Vec<Vec<u8>> = Vec::with_capacity(nalu_data.len());
    let mut conversion_applied = false;

    for (i, nalu) in nalu_data.iter().enumerate() {
        match DoviRpu::parse_unspec62_nalu(nalu) {
            Ok(mut rpu) => {
                if needs_conversion {
                    if let Err(e) = rpu.convert_with_mode(ConversionMode::To81) {
                        if i == 0 {
                            sink.log(&format!("  DV: Profile conversion warning: {e}"));
                        }
                    } else {
                        conversion_applied = true;
                    }
                }

                match rpu.write_hevc_unspec62_nalu() {
                    Ok(bytes) => rpu_payloads.push(bytes),
                    Err(e) => {
                        if i == 0 {
                            sink.log(&format!("  DV: RPU write error at frame {i}: {e}"));
                        }
                        rpu_payloads.push(nalu.clone());
                    }
                }
            }
            Err(e) => {
                if i == 0 {
                    sink.log(&format!("  DV: RPU parse error at frame {i}: {e}"));
                }
                rpu_payloads.push(nalu.clone());
            }
        }
    }

    let count = rpu_payloads.len();
    sink.log(&format!(
        "  DV: Extracted {} RPU{} (profile {}{})",
        count,
        if count == 1 { "" } else { "s" },
        dovi_profile,
        if conversion_applied { " → 8.1" } else { "" },
    ));

    Ok(ExtractedRpus {
        rpus: rpu_payloads,
        source_profile: dovi_profile,
        converted_to_81: conversion_applied,
    })
}

// ── RPU Injection + MP4Box Packaging ──────────────────────────────

/// Inject RPUs into an encoded HEVC file and package as DV-flagged MP4.
///
/// Uses streaming I/O: reads the encoded bitstream through a `NalReader`
/// and writes the injected bitstream through a `NalWriter`, so peak memory
/// is bounded by the largest single NAL unit (typically a few MB) rather
/// than the full file size.
#[cfg(feature = "dovi")]
pub async fn inject_and_package(
    encoded_path: &Path,
    original_source: &Path,
    final_mp4_path: &Path,
    rpus: &ExtractedRpus,
    sink: &dyn EventSink,
) -> Result<DoviPipelineResult, String> {
    // Pre-check: the DV pipeline creates temp files totalling ~2x the encoded
    // video size (demuxed HEVC + injected HEVC). Verify the temp partition
    // has enough space before starting.
    let encoded_size = std::fs::metadata(encoded_path)
        .map(|m| m.len())
        .unwrap_or(0);
    let needed = encoded_size.saturating_mul(3); // 2x video + margin for MP4Box

    // Place temp files near the output (avoids flatpak tmpfs and cross-device copies)
    let near = final_mp4_path.parent().unwrap_or(Path::new("."));
    if let Some((_total, free)) = crate::disk_monitor::partition_free_space(near) {
        if free < needed {
            return Err(format!(
                "Insufficient space for DV packaging: need ~{:.0}MB, \
                 only {:.0}MB free on {}",
                needed as f64 / 1_000_000.0,
                free as f64 / 1_000_000.0,
                near.display(),
            ));
        }
    }

    let temp_dir = make_tempdir("histv_dovi_inject_", Some(near))?;

    // Step 1: Demux encoded output to raw HEVC
    let encoded_hevc = temp_dir.path().join("encoded.h265");
    sink.log("  DV: Extracting encoded HEVC bitstream...");

    let demux_status = ffbin::ffmpeg_command()
        .args([
            "-y",
            "-i",
            &encoded_path.to_string_lossy(),
            "-c:v",
            "copy",
            "-bsf:v",
            "hevc_mp4toannexb",
            "-an",
            "-sn",
            "-f",
            "hevc",
            &encoded_hevc.to_string_lossy(),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map_err(|e| format!("Failed to demux encoded output: {e}"))?;

    if !demux_status.success() {
        return Err("Could not demux encoded output to raw HEVC".to_string());
    }

    // Step 2: Stream through encoded bitstream, inject RPU NALUs
    //
    // Frame boundaries are detected via `first_slice_segment_in_pic_flag`
    // (not AUDs, which libx265 doesn't emit by default). When we see the
    // start of a new picture — either a VCL NAL with first_slice=1 or a
    // non-VCL NAL after VCL NALs — we inject the RPU for the previous
    // picture before writing the current NAL.
    sink.log("  DV: Injecting RPU data into encoded bitstream (streaming)...");
    let injected_hevc = temp_dir.path().join("injected.h265");
    let rpu_count = rpus.rpus.len();
    let mut rpu_index: usize = 0;
    let mut in_frame = false;
    let rpus_ref = &rpus.rpus;

    if let Err(e) =
        hevc_utils::transform_bitstream(&encoded_hevc, &injected_hevc, |nalu, writer| {
            let is_new_picture = nalu.is_first_slice_of_picture();
            let is_non_vcl_after_vcl = in_frame && !nalu.is_vcl();

            // Inject RPU at picture boundary (end of previous frame's access unit)
            if (is_new_picture || is_non_vcl_after_vcl) && in_frame && rpu_index < rpus_ref.len() {
                writer.write_nalu(&rpus_ref[rpu_index])?;
                rpu_index += 1;
                in_frame = false;
            }

            writer.write_nalu(&nalu.data)?;

            if nalu.is_vcl() {
                in_frame = true;
            }
            Ok(())
        })
    {
        return Err(format!("RPU injection failed: {e}"));
    }

    // Inject trailing RPU if the stream ends mid-frame
    if in_frame && rpu_index < rpus_ref.len() {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&injected_hevc)
            .map_err(|e| format!("Could not append trailing RPU: {e}"))?;
        f.write_all(&hevc_utils::START_CODE)
            .and_then(|_| f.write_all(&rpus_ref[rpu_index]))
            .map_err(|e| format!("Write error: {e}"))?;
        rpu_index += 1;
    }

    // Delete the large intermediate encoded.h265 now that injection is done
    let _ = std::fs::remove_file(&encoded_hevc);

    if rpu_index < rpu_count {
        return Err(format!(
            "RPU count mismatch: {} RPUs extracted but only {} frames in encoded output. \
             DV metadata would be incomplete - falling back to HDR10.",
            rpu_count, rpu_index,
        ));
    }
    sink.log(&format!(
        "  DV: Injected {} RPUs into encoded bitstream",
        rpu_index
    ));

    // Step 3: Extract audio from original source
    let audio_aac = temp_dir.path().join("audio.aac");
    let has_audio = ffbin::ffmpeg_command()
        .args([
            "-y",
            "-i",
            &original_source.to_string_lossy(),
            "-vn",
            "-sn",
            "-c:a",
            "copy",
            &audio_aac.to_string_lossy(),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false);

    // Step 4: Package with MP4Box
    sink.log("  DV: Packaging with MP4Box (DV container flags)...");

    let (dv_profile_num, dv_compat) = if rpus.converted_to_81 {
        (8, 1)
    } else {
        match rpus.source_profile {
            5 => (5, 0),
            7 => (7, 6),
            8 => (8, 1),
            _ => (8, 1),
        }
    };

    let dv_profile_str = format!("dvhe.{:02}.{:02}", dv_profile_num, dv_compat);

    // GPAC 26.02 syntax: `:dvp=PROFILE.COMPAT` replaces the old
    // `:dv-profile=` / `:dv-bl-signal-comp-id=` flags.
    let mut mp4box_args: Vec<String> = vec![
        "-add".into(),
        format!(
            "{}#video:hdlr=vide:lang=und:group=1:dvp={}.{}",
            injected_hevc.display(),
            dv_profile_num,
            dv_compat,
        ),
    ];

    if has_audio && audio_aac.exists() {
        mp4box_args.push("-add".into());
        mp4box_args.push(format!(
            "{}#audio:hdlr=soun:lang=und:group=2",
            audio_aac.display()
        ));
    }

    mp4box_args.push("-brand".into());
    mp4box_args.push("mp42isom".into());
    mp4box_args.push("-ab".into());
    mp4box_args.push("dby1".into());
    mp4box_args.push("-no-iod".into());
    mp4box_args.push("-enable".into());
    mp4box_args.push("1".into());
    mp4box_args.push("-new".into());
    mp4box_args.push(final_mp4_path.to_string_lossy().to_string());

    let mp4box_output = crate::dovi_tools::mp4box_command()
        .args(&mp4box_args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("Failed to launch MP4Box: {e}"))?;

    drop(temp_dir); // clean up temp files before checking result

    if !mp4box_output.status.success() {
        let stderr = String::from_utf8_lossy(&mp4box_output.stderr);
        let err_detail = stderr
            .lines()
            .filter(|l| l.contains("Error") || l.contains("Failure") || l.contains("must indicate"))
            .collect::<Vec<_>>()
            .join("; ");
        let msg = if err_detail.is_empty() {
            "MP4Box packaging failed - falling back to HDR10".to_string()
        } else {
            format!("MP4Box failed: {} - falling back to HDR10", err_detail)
        };
        sink.log(&format!("  DV: {}", msg));
        return Ok(DoviPipelineResult {
            output_path: final_mp4_path.to_path_buf(),
            success: false,
            message: msg,
        });
    }

    sink.log(&format!(
        "  DV: Packaged as {} (profile {})",
        final_mp4_path.display(),
        dv_profile_str,
    ));

    Ok(DoviPipelineResult {
        output_path: final_mp4_path.to_path_buf(),
        success: true,
        message: format!("Dolby Vision preserved ({})", dv_profile_str),
    })
}

// ── Helpers ───────────────────────────────────────────────────────

/// Create a secure temp directory. Prefers `near` (same partition as
/// source/output) to avoid flatpak tmpfs space limits and cross-device copies.
/// Falls back to the system temp directory if `near` fails.
fn make_tempdir(prefix: &str, near: Option<&Path>) -> Result<TempDir, String> {
    if let Some(dir) = near {
        if let Ok(td) = tempfile::Builder::new().prefix(prefix).tempdir_in(dir) {
            return Ok(td);
        }
    }
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .map_err(|e| format!("Could not create temp dir: {e}"))
}

// ── Stubs for non-dovi builds ─────────────────────────────────────

#[cfg(not(feature = "dovi"))]
pub async fn extract_rpus(
    _source_path: &Path,
    _dovi_profile: u8,
    _dovi_bl_compat_id: Option<u8>,
    _sink: &dyn EventSink,
) -> Result<ExtractedRpus, String> {
    Err("Dolby Vision support not compiled in (missing 'dovi' feature)".to_string())
}

#[cfg(not(feature = "dovi"))]
pub async fn inject_and_package(
    _encoded_path: &Path,
    _original_source: &Path,
    _final_mp4_path: &Path,
    _rpus: &ExtractedRpus,
    _sink: &dyn EventSink,
) -> Result<DoviPipelineResult, String> {
    Err("Dolby Vision support not compiled in (missing 'dovi' feature)".to_string())
}
