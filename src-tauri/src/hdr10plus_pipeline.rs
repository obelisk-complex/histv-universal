//! HDR10+ dynamic metadata extract/inject pipeline.
//!
//! Tier 2 flow:
//! 1. Pre-encode: demux source to raw HEVC, extract HDR10+ SEI metadata
//! 2. Normal ffmpeg encode (unchanged)
//! 3. Post-encode: demux encoded output, inject HDR10+ SEI NALUs back
//!
//! All bitstream processing uses streaming I/O via `hevc_utils::NalReader`
//! to avoid loading multi-gigabyte files into memory.

#[cfg(feature = "dovi")]
use hdr10plus::hevc::encode_hdr10plus_nal;
#[cfg(feature = "dovi")]
use hdr10plus::metadata::Hdr10PlusMetadata;

use std::path::Path;

use crate::events::EventSink;
use crate::ffmpeg as ffbin;
use crate::hevc_utils;

/// Extracted HDR10+ metadata from a source file, pre-encoded as SEI NALUs
/// ready for injection. Encoding at extraction time avoids double-parsing.
pub struct ExtractedHdr10Plus {
    /// One pre-encoded SEI NAL unit per frame, ready to inject.
    pub sei_nalus: Vec<Vec<u8>>,
    /// Number of raw payloads that failed to encode (logged at extraction time).
    pub encode_failures: usize,
}

/// Result of HDR10+ injection.
pub struct Hdr10PlusPipelineResult {
    pub success: bool,
    pub message: String,
}

// ── HDR10+ SEI Constants ──────────────────────────────────────────

/// ITU-T T.35 country code for HDR10+ (United States)
const ITU_T35_COUNTRY_CODE_US: u8 = 0xB5;
/// Samsung HDR10+ provider code
const HDR10PLUS_PROVIDER_CODE: u16 = 0x003C;

// ── Extraction ────────────────────────────────────────────────────

/// Extract HDR10+ dynamic metadata from a source video file.
///
/// Demuxes to raw HEVC via ffmpeg, then streams through NAL units looking
/// for SEI_PREFIX NALs containing HDR10+ payloads. Only the small metadata
/// payloads are held in memory.
#[cfg(feature = "dovi")]
pub async fn extract_hdr10plus(
    source_path: &Path,
    sink: &dyn EventSink,
) -> Result<ExtractedHdr10Plus, String> {
    let temp_dir = hevc_utils::make_tempdir("histv_hdr10p_", source_path.parent())?;
    let raw_hevc = temp_dir.path().join("source.h265");

    sink.log("  HDR10+: Extracting HEVC bitstream from source...");
    hevc_utils::demux_to_annexb(source_path, &raw_hevc, sink, "HDR10+").await?;

    // Stream through the bitstream, extracting HDR10+ payloads from SEI NALs
    sink.log("  HDR10+: Scanning for dynamic metadata (streaming)...");
    let metadata = hevc_utils::extract_nalus_filtered(&raw_hevc, |nalu| {
        nalu.nal_type == hevc_utils::HEVC_NAL_SEI_PREFIX
    })
    .map_err(|e| format!("Failed to read bitstream: {e}"))?;

    drop(temp_dir); // clean up large demuxed file

    // Filter to HDR10+ payloads, parse, and pre-encode as SEI NALUs in one pass.
    // This avoids double-parsing (once here, once at injection).
    let mut sei_nalus: Vec<Vec<u8>> = Vec::new();
    let mut encode_failures = 0usize;
    let mut logged_sample = false;

    for sei_nalu in &metadata {
        let Some(payload) = parse_hdr10plus_from_sei_nalu(sei_nalu) else {
            continue;
        };
        match Hdr10PlusMetadata::parse(&payload) {
            Ok(m) => {
                if !logged_sample {
                    sink.log(&format!(
                        "  HDR10+: Detected dynamic metadata (profile {}, target {}nits)",
                        m.profile, m.targeted_system_display_maximum_luminance,
                    ));
                    logged_sample = true;
                }
                match encode_hdr10plus_nal(&m, false) {
                    Ok(nalu) => sei_nalus.push(nalu),
                    Err(_) => encode_failures += 1,
                }
            }
            Err(e) => {
                if !logged_sample {
                    sink.log(&format!("  HDR10+: Warning: first frame parse error: {e}"));
                    logged_sample = true;
                }
                encode_failures += 1;
            }
        }
    }

    if sei_nalus.is_empty() {
        return Err("No HDR10+ metadata found in bitstream".to_string());
    }

    if encode_failures > 0 {
        sink.log(&format!(
            "  HDR10+: Warning: {} metadata frames failed to encode",
            encode_failures,
        ));
    }

    sink.log(&format!(
        "  HDR10+: Pre-encoded {} SEI NALUs for injection",
        sei_nalus.len()
    ));

    Ok(ExtractedHdr10Plus {
        sei_nalus,
        encode_failures,
    })
}

/// Inject HDR10+ metadata back into an encoded HEVC file.
///
/// Streams through the encoded bitstream, inserting SEI_PREFIX NALs
/// before the first VCL NAL of each new picture (detected via
/// `first_slice_segment_in_pic_flag`). Then remuxes with the original
/// audio/subtitle tracks.
#[cfg(feature = "dovi")]
pub async fn inject_hdr10plus(
    encoded_path: &Path,
    output_path: &Path,
    metadata: &ExtractedHdr10Plus,
    sink: &dyn EventSink,
) -> Result<Hdr10PlusPipelineResult, String> {
    let temp_dir = hevc_utils::make_tempdir("histv_hdr10p_inject_", output_path.parent())?;

    // Step 1: Demux encoded output to raw HEVC
    let encoded_hevc = temp_dir.path().join("encoded.h265");
    sink.log("  HDR10+: Extracting encoded HEVC bitstream...");
    hevc_utils::demux_to_annexb(encoded_path, &encoded_hevc, sink, "HDR10+ inject").await?;

    // SEI NALUs were pre-encoded at extraction time — use directly
    let sei_nalus = &metadata.sei_nalus;
    if sei_nalus.is_empty() {
        return Err("No pre-encoded HDR10+ SEI NALUs available".to_string());
    }

    // Step 2: Stream through encoded bitstream, inject SEI before each new picture
    sink.log("  HDR10+: Injecting dynamic metadata (streaming)...");
    let injected_hevc = temp_dir.path().join("injected.h265");
    let mut sei_index: usize = 0;
    let sei_ref = sei_nalus;

    if let Err(e) =
        hevc_utils::transform_bitstream(&encoded_hevc, &injected_hevc, |nalu, writer| {
            // Inject SEI before the first slice of each new picture
            if nalu.is_first_slice_of_picture() && sei_index < sei_ref.len() {
                writer.write_nalu(&sei_ref[sei_index])?;
                sei_index += 1;
            }

            writer.write_nalu(&nalu.data)?;
            Ok(())
        })
    {
        return Err(format!("HDR10+ injection failed: {e}"));
    }

    let _ = std::fs::remove_file(&encoded_hevc);

    if sei_index < sei_nalus.len() {
        return Err(format!(
            "Metadata count mismatch: {} SEI frames available but only {} pictures in encoded output. \
             HDR10+ metadata would be incomplete - falling back to static HDR10.",
            sei_nalus.len(), sei_index,
        ));
    }

    // Step 3: Remux the injected bitstream with audio/subs from encoded output
    sink.log("  HDR10+: Remuxing with injected metadata...");
    let remux_output = ffbin::ffmpeg_command()
        .args([
            "-y",
            "-i",
            &injected_hevc.to_string_lossy(),
            "-i",
            &encoded_path.to_string_lossy(),
            "-map",
            "0:v:0",
            "-map",
            "1:a?",
            "-map",
            "1:s?",
            "-c",
            "copy",
            &output_path.to_string_lossy(),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to launch ffmpeg for remux: {e}"))?
        .wait_with_output()
        .await
        .map_err(|e| format!("Failed to remux: {e}"))?;

    drop(temp_dir); // clean up temp files

    if !remux_output.status.success() {
        let stderr = String::from_utf8_lossy(&remux_output.stderr);
        sink.log(&format!("  HDR10+ remux stderr: {stderr}"));
        return Ok(Hdr10PlusPipelineResult {
            success: false,
            message: "HDR10+ remux failed - output has static HDR10 only".to_string(),
        });
    }

    sink.log(&format!(
        "  HDR10+: Preserved {} dynamic metadata frames",
        sei_index
    ));

    Ok(Hdr10PlusPipelineResult {
        success: true,
        message: format!("HDR10+ preserved ({} frames)", sei_index),
    })
}

// ── Helpers ───────────────────────────────────────────────────────

/// Parse an HDR10+ ITU-T T.35 payload from a SEI_PREFIX NAL unit's data.
/// Returns the raw T.35 payload (including country code) if found.
fn parse_hdr10plus_from_sei_nalu(nalu_data: &[u8]) -> Option<Vec<u8>> {
    // Skip 2-byte NAL header
    if nalu_data.len() < 6 {
        return None;
    }
    let sei_data = &nalu_data[2..];

    let mut pos = 0;

    // Read payload type (multi-byte)
    let mut payload_type: u32 = 0;
    while pos < sei_data.len() && sei_data[pos] == 0xFF {
        payload_type += 255;
        pos += 1;
    }
    if pos >= sei_data.len() {
        return None;
    }
    payload_type += sei_data[pos] as u32;
    pos += 1;

    // SEI type 4 = user_data_registered_itu_t_t35
    if payload_type != 4 {
        return None;
    }

    // Read payload size (multi-byte)
    let mut payload_size: u32 = 0;
    while pos < sei_data.len() && sei_data[pos] == 0xFF {
        payload_size += 255;
        pos += 1;
    }
    if pos >= sei_data.len() {
        return None;
    }
    payload_size += sei_data[pos] as u32;
    pos += 1;

    let payload_end = pos + payload_size as usize;
    if payload_end > sei_data.len() {
        return None;
    }

    let payload = &sei_data[pos..payload_end];
    if payload.len() < 5 {
        return None;
    }

    // Check ITU-T T.35 country code + Samsung provider code
    if payload[0] != ITU_T35_COUNTRY_CODE_US {
        return None;
    }
    let provider = u16::from_be_bytes([payload[1], payload[2]]);
    if provider != HDR10PLUS_PROVIDER_CODE {
        return None;
    }

    Some(payload.to_vec())
}

// ── Stubs for non-dovi builds ─────────────────────────────────────

#[cfg(not(feature = "dovi"))]
pub async fn extract_hdr10plus(
    _source_path: &Path,
    _sink: &dyn EventSink,
) -> Result<ExtractedHdr10Plus, String> {
    Err("HDR10+ support not compiled in (missing 'dovi' feature)".to_string())
}

#[cfg(not(feature = "dovi"))]
pub async fn inject_hdr10plus(
    _encoded_path: &Path,
    _output_path: &Path,
    _metadata: &ExtractedHdr10Plus,
    _sink: &dyn EventSink,
) -> Result<Hdr10PlusPipelineResult, String> {
    Err("HDR10+ support not compiled in (missing 'dovi' feature)".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal SEI_PREFIX NAL with an HDR10+ T.35 payload.
    fn build_hdr10plus_sei_nalu(payload: &[u8]) -> Vec<u8> {
        // 2-byte NAL header for SEI_PREFIX (type 39): (39 << 1) | 0 = 0x4E, then 0x01
        let mut nalu = vec![0x4E, 0x01];
        // SEI payload type 4 (user_data_registered_itu_t_t35)
        nalu.push(4);
        // SEI payload size (single byte for small payloads)
        nalu.push(payload.len() as u8);
        nalu.extend_from_slice(payload);
        nalu
    }

    #[test]
    fn parse_valid_hdr10plus_sei() {
        // Country code US (0xB5) + Samsung provider (0x003C) + dummy data
        let payload = vec![
            ITU_T35_COUNTRY_CODE_US,
            0x00,
            0x3C, // provider code
            0x00,
            0x01, // provider-oriented code (HDR10+)
            0xAA,
            0xBB,
            0xCC, // payload data
        ];
        let nalu = build_hdr10plus_sei_nalu(&payload);
        let result = parse_hdr10plus_from_sei_nalu(&nalu);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), payload);
    }

    #[test]
    fn parse_rejects_wrong_country_code() {
        let payload = vec![
            0x00, // wrong country code
            0x00, 0x3C, 0x00, 0x01, 0xAA, 0xBB, 0xCC,
        ];
        let nalu = build_hdr10plus_sei_nalu(&payload);
        assert!(parse_hdr10plus_from_sei_nalu(&nalu).is_none());
    }

    #[test]
    fn parse_rejects_wrong_provider() {
        let payload = vec![
            ITU_T35_COUNTRY_CODE_US,
            0x00,
            0xFF, // wrong provider
            0x00,
            0x01,
            0xAA,
            0xBB,
            0xCC,
        ];
        let nalu = build_hdr10plus_sei_nalu(&payload);
        assert!(parse_hdr10plus_from_sei_nalu(&nalu).is_none());
    }

    #[test]
    fn parse_rejects_non_t35_sei_type() {
        // SEI type 5 (user_data_unregistered) instead of type 4
        let mut nalu = vec![0x4E, 0x01];
        nalu.push(5); // wrong type
        nalu.push(8); // size
        nalu.extend_from_slice(&[
            ITU_T35_COUNTRY_CODE_US,
            0x00,
            0x3C,
            0x00,
            0x01,
            0xAA,
            0xBB,
            0xCC,
        ]);
        assert!(parse_hdr10plus_from_sei_nalu(&nalu).is_none());
    }

    #[test]
    fn parse_rejects_too_short() {
        assert!(parse_hdr10plus_from_sei_nalu(&[]).is_none());
        assert!(parse_hdr10plus_from_sei_nalu(&[0x4E, 0x01, 4, 2, 0xB5, 0x00]).is_none());
    }
}
