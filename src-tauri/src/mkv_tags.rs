//! MKV stream statistics tag repair.
//!
//! MKV files can carry per-stream metadata tags (BPS, NUMBER_OF_BYTES,
//! DURATION, NUMBER_OF_FRAMES) written by the original muxing tool.
//! ffmpeg copies these verbatim during re-encoding, leaving stale values
//! that mislead probes into reporting the wrong bitrate.
//!
//! This module provides two repair tiers:
//!
//! **Lightweight (automatic)** - runs before every encoding batch and
//! after each encode. Computes video BPS and NUMBER_OF_BYTES from the
//! actual file size minus estimated audio/subtitle contribution, and
//! patches DURATION from the probed value. Instant on any hardware.
//!
//! **Deep (manual)** - scans every packet in the file via ffprobe to
//! get exact per-stream byte counts and frame counts, then patches all
//! statistics tags with precise values. Slower (reads the full file)
//! but produces byte-accurate results.
//!
//! Both tiers patch values in-place, right-padded with spaces to
//! preserve EBML element sizes. Only MKV files are affected.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

// ── EBML element IDs ───────────────────────────────────────────

const ID_SEGMENT: u32 = 0x18538067;
const ID_TAGS: u32 = 0x1254C367;
const ID_TAG: u32 = 0x7373;
const ID_SIMPLE_TAG: u32 = 0x67C8;
const ID_TAG_NAME: u32 = 0x45A3;
const ID_TAG_STRING: u32 = 0x4487;
const ID_SEEKHEAD: u32 = 0x114D9B74;
const ID_SEEK: u32 = 0x4DBB;
const ID_SEEKID: u32 = 0x53AB;
const ID_SEEKPOSITION: u32 = 0x53AC;

// ── EBML primitives ────────────────────────────────────────────

struct ElementHeader {
    id: u32,
    data_size: u64,
    header_len: u64,
    unknown_size: bool,
}

fn read_element_id(r: &mut impl Read) -> Result<(u32, usize), String> {
    let mut first = [0u8; 1];
    r.read_exact(&mut first)
        .map_err(|e| format!("read ID: {e}"))?;
    let b = first[0];
    if b == 0 {
        return Err("invalid EBML ID (zero byte)".into());
    }

    let len = b.leading_zeros() as usize + 1;
    if len > 4 {
        return Err("invalid EBML ID (>4 bytes)".into());
    }

    let mut id = b as u32;
    if len > 1 {
        let mut rest = vec![0u8; len - 1];
        r.read_exact(&mut rest)
            .map_err(|e| format!("read ID: {e}"))?;
        for &byte in &rest {
            id = (id << 8) | byte as u32;
        }
    }
    Ok((id, len))
}

fn read_vint(r: &mut impl Read) -> Result<(u64, usize, bool), String> {
    let mut first = [0u8; 1];
    r.read_exact(&mut first)
        .map_err(|e| format!("read VINT: {e}"))?;
    let b = first[0];
    if b == 0 {
        return Err("invalid VINT (zero byte)".into());
    }

    let len = b.leading_zeros() as usize + 1;
    if len > 8 {
        return Err("invalid VINT (>8 bytes)".into());
    }

    let mask = if len >= 8 { 0u8 } else { 0xFFu8 >> len };
    let mut value = (b & mask) as u64;
    let mut all_ones = (b & mask) == mask;

    if len > 1 {
        let mut rest = [0u8; 7]; // VINT is at most 8 bytes; first already read
        r.read_exact(&mut rest[..len - 1])
            .map_err(|e| format!("read VINT: {e}"))?;
        for &byte in &rest[..len - 1] {
            value = (value << 8) | byte as u64;
            if byte != 0xFF {
                all_ones = false;
            }
        }
    }

    Ok((value, len, all_ones))
}

fn read_element_header(r: &mut (impl Read + Seek)) -> Result<ElementHeader, String> {
    let (id, id_len) = read_element_id(r)?;
    let (data_size, size_len, unknown_size) = read_vint(r)?;
    Ok(ElementHeader {
        id,
        data_size,
        header_len: (id_len + size_len) as u64,
        unknown_size,
    })
}

fn read_string(r: &mut impl Read, len: u64) -> Result<String, String> {
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf)
        .map_err(|e| format!("read string: {e}"))?;
    if let Some(null_pos) = buf.iter().position(|&b| b == 0) {
        buf.truncate(null_pos);
    }
    String::from_utf8(buf).map_err(|e| format!("invalid UTF-8: {e}"))
}

fn read_uint(r: &mut impl Read, len: u64) -> Result<u64, String> {
    let mut val: u64 = 0;
    for _ in 0..len {
        let mut b = [0u8; 1];
        r.read_exact(&mut b)
            .map_err(|e| format!("read uint: {e}"))?;
        val = (val << 8) | b[0] as u64;
    }
    Ok(val)
}

// ── Tag location tracking ──────────────────────────────────────

struct TagLocation {
    name: String,
    value_offset: u64,
    value_length: usize,
}

/// All statistics tag names we know how to patch.
const STATS_TAG_NAMES: &[&str] = &[
    "BPS",
    "BPS-eng",
    "NUMBER_OF_BYTES",
    "NUMBER_OF_BYTES-eng",
    "DURATION",
    "DURATION-eng",
    "NUMBER_OF_FRAMES",
    "NUMBER_OF_FRAMES-eng",
];

// ── Navigation helpers ─────────────────────────────────────────

fn find_child(
    file: &mut std::fs::File,
    target_id: u32,
    parent_data_end: u64,
) -> Result<Option<ElementHeader>, String> {
    loop {
        let pos = file.stream_position().map_err(|e| format!("{e}"))?;
        if pos >= parent_data_end {
            return Ok(None);
        }

        let header = match read_element_header(file) {
            Ok(h) => h,
            Err(_) => return Ok(None),
        };

        if header.id == target_id {
            return Ok(Some(header));
        }

        let skip_to = pos + header.header_len + header.data_size;
        if skip_to > parent_data_end {
            return Ok(None);
        }
        file.seek(SeekFrom::Start(skip_to))
            .map_err(|e| format!("seek: {e}"))?;
    }
}

fn parse_simple_tag(
    file: &mut std::fs::File,
    simple_tag_data_end: u64,
) -> Result<Option<(String, u64, usize)>, String> {
    let mut tag_name: Option<String> = None;
    let mut string_offset: Option<u64> = None;
    let mut string_length: Option<usize> = None;

    loop {
        let pos = file.stream_position().map_err(|e| format!("{e}"))?;
        if pos >= simple_tag_data_end {
            break;
        }

        let header = match read_element_header(file) {
            Ok(h) => h,
            Err(_) => break,
        };

        let data_start = file.stream_position().map_err(|e| format!("{e}"))?;

        if header.id == ID_TAG_NAME {
            tag_name = Some(read_string(file, header.data_size)?);
        } else if header.id == ID_TAG_STRING {
            string_offset = Some(data_start);
            string_length = Some(header.data_size as usize);
            file.seek(SeekFrom::Start(data_start + header.data_size))
                .map_err(|e| format!("seek: {e}"))?;
        } else {
            file.seek(SeekFrom::Start(data_start + header.data_size))
                .map_err(|e| format!("seek: {e}"))?;
        }
    }

    match (tag_name, string_offset, string_length) {
        (Some(name), Some(offset), Some(length)) => Ok(Some((name, offset, length))),
        _ => Ok(None),
    }
}

fn collect_tag_locations(
    file: &mut std::fs::File,
    tag_data_end: u64,
) -> Result<Vec<TagLocation>, String> {
    let mut locations = Vec::new();

    loop {
        let pos = file.stream_position().map_err(|e| format!("{e}"))?;
        if pos >= tag_data_end {
            break;
        }

        let header = match read_element_header(file) {
            Ok(h) => h,
            Err(_) => break,
        };

        let data_start = file.stream_position().map_err(|e| format!("{e}"))?;
        let data_end = data_start + header.data_size;

        if header.id == ID_SIMPLE_TAG {
            if let Some((name, offset, length)) = parse_simple_tag(file, data_end)? {
                if STATS_TAG_NAMES.iter().any(|&s| s == name) {
                    locations.push(TagLocation {
                        name,
                        value_offset: offset,
                        value_length: length,
                    });
                }
            }
        }

        file.seek(SeekFrom::Start(data_end))
            .map_err(|e| format!("seek: {e}"))?;
    }

    Ok(locations)
}

// ── SeekHead navigation ────────────────────────────────────────

fn find_tags_via_seekhead(
    file: &mut std::fs::File,
    segment_data_start: u64,
    segment_data_end: u64,
) -> Result<Option<u64>, String> {
    file.seek(SeekFrom::Start(segment_data_start))
        .map_err(|e| format!("seek: {e}"))?;

    let seekhead = match find_child(file, ID_SEEKHEAD, segment_data_end)? {
        Some(h) => h,
        None => return Ok(None),
    };

    let seekhead_data_start = file.stream_position().map_err(|e| format!("{e}"))?;
    let seekhead_data_end = seekhead_data_start + seekhead.data_size;

    loop {
        let seek_header = match find_child(file, ID_SEEK, seekhead_data_end)? {
            Some(h) => h,
            None => return Ok(None),
        };

        let seek_data_start = file.stream_position().map_err(|e| format!("{e}"))?;
        let seek_data_end = seek_data_start + seek_header.data_size;

        let mut found_id: Option<u32> = None;
        let mut found_pos: Option<u64> = None;

        loop {
            let pos = file.stream_position().map_err(|e| format!("{e}"))?;
            if pos >= seek_data_end {
                break;
            }

            let child = match read_element_header(file) {
                Ok(h) => h,
                Err(_) => break,
            };

            let child_data_start = file.stream_position().map_err(|e| format!("{e}"))?;

            if child.id == ID_SEEKID {
                found_id = Some(read_uint(file, child.data_size)? as u32);
            } else if child.id == ID_SEEKPOSITION {
                found_pos = Some(read_uint(file, child.data_size)?);
            }

            file.seek(SeekFrom::Start(child_data_start + child.data_size))
                .map_err(|e| format!("seek: {e}"))?;
        }

        if found_id == Some(ID_TAGS) {
            if let Some(pos) = found_pos {
                return Ok(Some(segment_data_start + pos));
            }
        }

        file.seek(SeekFrom::Start(seek_data_end))
            .map_err(|e| format!("seek: {e}"))?;
    }
}

// ── Shared EBML open + navigate to Tags ────────────────────────

/// Open an MKV file and navigate to the Tags element via SeekHead.
/// Returns the open file, the Tags data range, and the segment data start.
fn open_and_find_tags(path: &Path) -> Result<Option<(std::fs::File, u64, u64)>, String> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| format!("open: {e}"))?;

    // Skip EBML header
    let ebml_header = read_element_header(&mut file)?;
    file.seek(SeekFrom::Current(ebml_header.data_size as i64))
        .map_err(|e| format!("seek past EBML header: {e}"))?;

    // Read Segment header
    let segment = read_element_header(&mut file)?;
    if segment.id != ID_SEGMENT {
        return Err("not a valid MKV (Segment element not found)".into());
    }
    let segment_data_start = file.stream_position().map_err(|e| format!("{e}"))?;
    let segment_data_end = if segment.unknown_size {
        file.seek(SeekFrom::End(0)).map_err(|e| format!("{e}"))?
    } else {
        segment_data_start + segment.data_size
    };

    // Use SeekHead to find Tags
    let tags_offset = match find_tags_via_seekhead(&mut file, segment_data_start, segment_data_end)?
    {
        Some(offset) => offset,
        None => return Ok(None),
    };

    file.seek(SeekFrom::Start(tags_offset))
        .map_err(|e| format!("seek to Tags: {e}"))?;

    let tags_header = read_element_header(&mut file)?;
    if tags_header.id != ID_TAGS {
        return Ok(None);
    }
    let tags_data_start = file.stream_position().map_err(|e| format!("{e}"))?;
    let tags_data_end = tags_data_start + tags_header.data_size;

    Ok(Some((file, tags_data_start, tags_data_end)))
}

// ── Formatting helpers ─────────────────────────────────────────

/// Format seconds as MKV duration string "HH:MM:SS.mmmmmmmmm".
fn format_mkv_duration(secs: f64) -> String {
    let total_nanos = (secs * 1_000_000_000.0) as u64;
    let h = total_nanos / 3_600_000_000_000;
    let m = (total_nanos % 3_600_000_000_000) / 60_000_000_000;
    let s = (total_nanos % 60_000_000_000) / 1_000_000_000;
    let ns = total_nanos % 1_000_000_000;
    format!("{:02}:{:02}:{:02}.{:09}", h, m, s, ns)
}

// ── Patching engine ────────────────────────────────────────────

/// Values to write into the statistics tags.
pub struct TagValues {
    pub bps: Option<u64>,
    pub number_of_bytes: Option<u64>,
    pub duration_secs: Option<f64>,
    pub number_of_frames: Option<u64>,
}

/// Patch the first Tag element in an MKV file that contains statistics
/// tags. Values are right-padded with spaces to preserve EBML sizes.
/// Returns the number of individual tag values patched.
fn patch_first_statistics_tag(path: &Path, values: &TagValues) -> Result<u32, String> {
    let (mut file, tags_data_start, tags_data_end) = match open_and_find_tags(path)? {
        Some(t) => t,
        None => return Ok(0),
    };

    // Find the first Tag with statistics
    file.seek(SeekFrom::Start(tags_data_start))
        .map_err(|e| format!("seek: {e}"))?;

    let mut locations: Vec<TagLocation> = Vec::new();

    loop {
        let tag_header = match find_child(&mut file, ID_TAG, tags_data_end)? {
            Some(h) => h,
            None => break,
        };
        let tag_data_start = file.stream_position().map_err(|e| format!("{e}"))?;
        let tag_data_end = tag_data_start + tag_header.data_size;

        let found = collect_tag_locations(&mut file, tag_data_end)?;
        if !found.is_empty() {
            locations = found;
            break;
        }

        file.seek(SeekFrom::Start(tag_data_end))
            .map_err(|e| format!("seek: {e}"))?;
    }

    if locations.is_empty() {
        return Ok(0);
    }

    // Patch each tag
    let mut patched: u32 = 0;
    let duration_str = values.duration_secs.map(format_mkv_duration);

    for loc in &locations {
        let base_name = loc.name.trim_end_matches("-eng");
        let new_value = match base_name {
            "BPS" => values.bps.map(|v| v.to_string()),
            "NUMBER_OF_BYTES" => values.number_of_bytes.map(|v| v.to_string()),
            "DURATION" => duration_str.clone(),
            "NUMBER_OF_FRAMES" => values.number_of_frames.map(|v| v.to_string()),
            _ => None,
        };

        let new_value = match new_value {
            Some(v) => v,
            None => continue,
        };

        // Can't patch in-place if new value is longer than the existing field
        if new_value.len() > loc.value_length {
            continue;
        }

        let padded = if new_value.len() < loc.value_length {
            format!("{:<width$}", new_value, width = loc.value_length)
        } else {
            new_value
        };

        file.seek(SeekFrom::Start(loc.value_offset))
            .map_err(|e| format!("seek to patch: {e}"))?;
        file.write_all(padded.as_bytes())
            .map_err(|e| format!("write patch: {e}"))?;
        patched += 1;
    }

    file.flush().map_err(|e| format!("flush: {e}"))?;

    Ok(patched)
}

// ── Public API ─────────────────────────────────────────────────

/// Lightweight tag repair: computes correct values from file size and
/// probe data, then patches the video stream's statistics tags in-place.
///
/// Called automatically before every encoding batch and after each
/// encode. Computation is trivial (arithmetic on file size, duration,
/// and audio bitrates) - the only I/O is the in-place tag write.
///
/// `frame_count` is the exact number of video frames, if known (e.g.
/// from ffmpeg's stderr output during encoding). Pass None to skip
/// patching NUMBER_OF_FRAMES.
///
/// Returns (patched_count, computed_video_bps).
pub fn lightweight_repair(
    path: &Path,
    file_size: u64,
    duration_secs: f64,
    audio_bitrate_total_bps: u64,
    frame_count: Option<u64>,
) -> Result<(u32, u64), String> {
    if duration_secs <= 0.0 {
        return Ok((0, 0));
    }

    let audio_bytes = (audio_bitrate_total_bps as f64 * duration_secs / 8.0) as u64;
    let video_bytes = file_size.saturating_sub(audio_bytes);
    let video_bps = (video_bytes as f64 * 8.0 / duration_secs) as u64;

    let patched = patch_first_statistics_tag(
        path,
        &TagValues {
            bps: Some(video_bps),
            number_of_bytes: Some(video_bytes),
            duration_secs: Some(duration_secs),
            number_of_frames: frame_count,
        },
    )?;

    Ok((patched, video_bps))
}

/// Probe-and-repair: checks if a file is MKV with valid duration, then runs
/// `lightweight_repair` and returns the corrected video bitrate if any tags
/// were updated. Encapsulates the pattern duplicated across lib.rs, cli_main.rs,
/// and encoder.rs.
///
/// Returns `Some(corrected_video_bps)` if tags were patched, `None` otherwise.
pub fn repair_after_probe(
    file_path: &str,
    duration_secs: f64,
    audio_streams: &[crate::queue::AudioStreamInfo],
) -> Option<u64> {
    if !file_path.ends_with(".mkv") || duration_secs <= 0.0 {
        return None;
    }
    let file_size = std::fs::metadata(file_path).map(|m| m.len()).ok()?;
    let audio_total_bps: u64 = audio_streams
        .iter()
        .map(|s| s.bitrate_kbps as u64 * 1000)
        .sum();
    match lightweight_repair(
        std::path::Path::new(file_path),
        file_size,
        duration_secs,
        audio_total_bps,
        None,
    ) {
        Ok((n, bps)) if n > 0 => Some(bps),
        _ => None,
    }
}

/// Post-encode tag update. Convenience wrapper around `lightweight_repair`
/// for use immediately after encoding, when file size and probe data are
/// already available.
pub fn update_video_stream_tags(
    path: &Path,
    video_bps: u64,
    video_bytes: u64,
) -> Result<u32, String> {
    patch_first_statistics_tag(
        path,
        &TagValues {
            bps: Some(video_bps),
            number_of_bytes: Some(video_bytes),
            duration_secs: None,
            number_of_frames: None,
        },
    )
}

/// Deep tag repair: computes exact per-stream byte counts by scanning
/// the small audio and subtitle streams (seconds, not minutes), then
/// subtracts them from the file size to derive exact video bytes. Frame
/// count is obtained from a single ffprobe call.
///
/// Faster than scanning every video packet (which would output millions
/// of JSON entries for a 4K file) while still being byte-accurate for
/// the video stream. Audio and subtitle streams are small enough that
/// their packet scans complete in seconds.
///
/// Returns (patched_count, computed_video_bps).
pub async fn deep_repair(
    path: &Path,
    sink: &dyn crate::events::EventSink,
) -> Result<(u32, u64), String> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if ext != "mkv" {
        return Ok((0, 0));
    }

    let path_str = path.to_string_lossy().to_string();
    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    sink.log(&format!("[repair] Deep scan: {}", file_name));

    // Get duration and stream info from probe
    let probe = crate::probe::probe_file(&path_str, sink).await?;
    if probe.duration_secs <= 0.0 {
        return Err("could not determine file duration".into());
    }

    let file_size = std::fs::metadata(path)
        .map(|m| m.len())
        .map_err(|e| format!("stat: {e}"))?;

    // Count how many audio and subtitle streams exist
    let audio_count = probe.audio_streams.len();

    // Scan audio stream bytes (fast - audio is small relative to video)
    let mut non_video_bytes: u64 = 0;
    for i in 0..audio_count {
        let selector = format!("a:{}", i);
        sink.log(&format!("[repair] Scanning audio stream {}...", i));
        match packet_scan_stream_bytes(path, &selector).await {
            Ok(bytes) => non_video_bytes += bytes,
            Err(e) => sink.log(&format!("[repair] WARNING: Audio {} scan failed: {}", i, e)),
        }
    }

    // Scan subtitle stream bytes (fast - subs are tiny)
    // Try up to 8 subtitle streams (generous upper bound)
    for i in 0..8 {
        let selector = format!("s:{}", i);
        match packet_scan_stream_bytes(path, &selector).await {
            Ok(0) => break, // No more subtitle streams
            Ok(bytes) => {
                sink.log(&format!("[repair] Subtitle stream {}: {} bytes", i, bytes));
                non_video_bytes += bytes;
            }
            Err(_) => break, // Stream doesn't exist
        }
    }

    // Video bytes = file size minus all non-video content
    let video_bytes = file_size.saturating_sub(non_video_bytes);
    let video_bps = (video_bytes as f64 * 8.0 / probe.duration_secs) as u64;

    // Frame count via ffmpeg -c copy -f null, with live progress.
    let video_frames = count_frames_with_progress(path, probe.duration_secs, sink)
        .await
        .unwrap_or(0);

    let patched = patch_first_statistics_tag(
        path,
        &TagValues {
            bps: Some(video_bps),
            number_of_bytes: Some(video_bytes),
            duration_secs: Some(probe.duration_secs),
            number_of_frames: if video_frames > 0 {
                Some(video_frames)
            } else {
                None
            },
        },
    )?;

    let video_mbps = video_bps as f64 / 1_000_000.0;
    sink.log(&format!(
        "[repair] Deep scan complete: {:.2}Mbps, {} bytes, {} frames (non-video: {} bytes)",
        video_mbps, video_bytes, video_frames, non_video_bytes
    ));

    Ok((patched, video_bps))
}

/// Lightweight repair using probe data. Convenience wrapper for the
/// automatic pre-batch and post-encode repair paths.
///
/// Returns (patched_count, computed_video_bps).
pub async fn repair_file_tags(
    path: &Path,
    sink: &dyn crate::events::EventSink,
) -> Result<(u32, u64), String> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if ext != "mkv" {
        return Ok((0, 0));
    }

    let path_str = path.to_string_lossy().to_string();
    let probe = crate::probe::probe_file(&path_str, sink).await?;

    if probe.duration_secs <= 0.0 {
        return Err("could not determine file duration".into());
    }

    let file_size = std::fs::metadata(path)
        .map(|m| m.len())
        .map_err(|e| format!("stat: {e}"))?;

    let audio_total_bps: u64 = probe
        .audio_streams
        .iter()
        .map(|s| s.bitrate_kbps as u64 * 1000)
        .sum();

    lightweight_repair(path, file_size, probe.duration_secs, audio_total_bps, None)
}

// ── ffprobe packet scanning helpers ────────────────────────────

/// Sum all packet sizes for a given stream selector (e.g. "v:0", "a:0").
async fn packet_scan_stream_bytes(path: &Path, stream_selector: &str) -> Result<u64, String> {
    let raw = crate::probe::run_ffprobe_public(&[
        "-v",
        "error",
        "-select_streams",
        stream_selector,
        "-show_entries",
        "packet=size",
        "-of",
        "json",
        &path.to_string_lossy(),
    ])
    .await?;

    let json: serde_json::Value = serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);

    let total: u64 = json["packets"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|p| p["size"].as_str().and_then(|s| s.parse::<u64>().ok()))
                .sum()
        })
        .unwrap_or(0);

    Ok(total)
}

/// Count video frames by running `ffmpeg -c copy -f null -`, which reads
/// every packet without decoding and outputs `frame=` and `time=` on
/// stderr. We parse these for a live progress percentage and the final
/// frame count. Much better UX than the silent ffprobe -count_frames.
async fn count_frames_with_progress(
    path: &Path,
    duration_secs: f64,
    sink: &dyn crate::events::EventSink,
) -> Result<u64, String> {
    let path_str = path.to_string_lossy().to_string();

    let mut cmd = crate::ffmpeg::ffmpeg_command();
    cmd.args([
        "-v", "error", "-stats", "-i", &path_str, "-map", "0:v:0", "-c", "copy", "-f", "null", "-",
    ])
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to launch ffmpeg for frame count: {e}"))?;

    let stderr = child.stderr.take();
    let progress = crate::encoder::FfmpegProgress::new();
    let log_dir = path.parent().unwrap_or(std::path::Path::new("."));
    let stderr_log = crate::encoder::open_stderr_log(log_dir);
    let stderr_thread = stderr.map(|stderr| {
        crate::encoder::spawn_stderr_reader(stderr, &progress, stderr_log.clone(), "mkv_tags")
    });

    // Poll for progress updates
    let mut last_pct: i32 = -1;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(_) => break,
        }

        if duration_secs > 0.0 {
            let secs = progress.secs();
            let pct = ((secs / duration_secs) * 100.0).min(100.0) as i32;
            let pct_bucket = pct / 10 * 10; // Round down to nearest 10%
            if pct_bucket != last_pct && pct_bucket > 0 {
                let fc = progress.frames();
                sink.log(&format!(
                    "[repair] Counting frames: {}% ({} frames)",
                    pct_bucket, fc
                ));
                last_pct = pct_bucket;
            }
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    if let Some(handle) = stderr_thread {
        let _ = handle.join();
    }

    // Clean up: drop the shared log handle, then remove if empty and enforce cap
    drop(stderr_log);
    crate::encoder::cleanup_stderr_logs(log_dir, None, 10);

    let final_count = progress.frames();
    Ok(final_count)
}
