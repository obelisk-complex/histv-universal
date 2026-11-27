//! Animated WebP decoding via RIFF container parsing and ffmpeg frame decode.
//!
//! ffmpeg's built-in WebP decoder does not support animated WebP (ANIM/ANMF
//! chunks). This module works around the limitation by:
//!
//! 1. Parsing the RIFF container to extract canvas dimensions, frame count,
//!    total duration, and each frame's compressed payload.
//! 2. Feeding each frame's raw WebP bytes into a single ffmpeg process via
//!    `image2pipe` stdin, which decodes them using the static WebP decoder.
//! 3. Reading decoded RGBA pixels from ffmpeg's stdout.
//! 4. Compositing each frame onto a canvas according to the WebP animation
//!    spec (x/y offset, disposal method, alpha blending).
//! 5. Piping the composited canvas to a second ffmpeg process that encodes
//!    the final output video.
//!
//! The result is correct animated WebP to video conversion with no external
//! dependencies beyond ffmpeg itself, and per-frame progress reporting.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::events::{BatchControl, EventSink};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

// ── Public types ───────────────────────────────────────────────

/// Metadata extracted from an animated WebP file's container.
#[derive(Debug, Clone)]
pub struct WebpInfo {
    pub width: u32,
    pub height: u32,
    pub frame_count: u32,
    pub total_duration_ms: u32,
    pub loop_count: u16,
    pub has_alpha: bool,
}

/// A single animation frame's metadata and compressed data.
#[derive(Debug)]
struct AnimFrame {
    x_offset: u32,
    y_offset: u32,
    width: u32,
    height: u32,
    duration_ms: u32,
    dispose: bool, // true = dispose to background after rendering
    blend: bool,   // true = alpha-blend onto canvas; false = overwrite
    data: Vec<u8>, // raw WebP bitstream (VP8/VP8L, possibly with ALPH)
}

// ── Container parsing ──────────────────────────────────────────

/// Read a 4-byte ASCII chunk ID.
fn read_fourcc(r: &mut impl Read) -> Result<[u8; 4], String> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf)
        .map_err(|e| format!("read fourcc: {e}"))?;
    Ok(buf)
}

/// Read a 32-bit little-endian unsigned integer.
fn read_u32_le(r: &mut impl Read) -> Result<u32, String> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf)
        .map_err(|e| format!("read u32: {e}"))?;
    Ok(u32::from_le_bytes(buf))
}

/// Read a 24-bit little-endian unsigned integer (3 bytes).
fn read_u24_le(r: &mut impl Read) -> Result<u32, String> {
    let mut buf = [0u8; 3];
    r.read_exact(&mut buf)
        .map_err(|e| format!("read u24: {e}"))?;
    Ok(buf[0] as u32 | (buf[1] as u32) << 8 | (buf[2] as u32) << 16)
}

/// Read a 16-bit little-endian unsigned integer.
fn read_u16_le(r: &mut impl Read) -> Result<u16, String> {
    let mut buf = [0u8; 2];
    r.read_exact(&mut buf)
        .map_err(|e| format!("read u16: {e}"))?;
    Ok(u16::from_le_bytes(buf))
}

/// Probe an animated WebP for metadata only, without reading frame data.
/// Returns None if the file is not an animated WebP (no ANIM chunk found).
fn probe_metadata(path: &Path) -> Result<Option<WebpInfo>, String> {
    let mut file =
        std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;

    file.seek(SeekFrom::Start(12))
        .map_err(|e| format!("seek: {e}"))?;

    let mut width: u32 = 0;
    let mut height: u32 = 0;
    let mut has_alpha = false;
    let mut loop_count: u16 = 0;
    let mut total_duration_ms: u32 = 0;
    let mut frame_count: u32 = 0;
    let mut found_anim = false;

    while let Ok(chunk_id) = read_fourcc(&mut file) {
        let chunk_size = match read_u32_le(&mut file) {
            Ok(s) => s,
            Err(_) => break,
        };
        let chunk_start = file.stream_position().map_err(|e| format!("{e}"))?;

        match &chunk_id {
            b"VP8X" => {
                let mut flags = [0u8; 4];
                file.read_exact(&mut flags)
                    .map_err(|e| format!("VP8X: {e}"))?;
                has_alpha = (flags[0] & 0x10) != 0;
                width = read_u24_le(&mut file)? + 1;
                height = read_u24_le(&mut file)? + 1;
            }
            b"ANIM" => {
                let _bg = read_u32_le(&mut file)?;
                loop_count = read_u16_le(&mut file)?;
                found_anim = true;
            }
            b"ANMF" => {
                // Read only the duration (3 bytes at offset +12 into the chunk)
                // Skip: x(3) + y(3) + w(3) + h(3) = 12 bytes
                file.seek(SeekFrom::Start(chunk_start + 12))
                    .map_err(|e| format!("seek: {e}"))?;
                let dur = read_u24_le(&mut file)?;
                total_duration_ms = total_duration_ms.saturating_add(dur);
                frame_count += 1;
                // Skip the rest - no frame data read
            }
            _ => {}
        }

        let padded_size = chunk_size as u64 + (chunk_size as u64 & 1);
        let next = chunk_start + padded_size;
        file.seek(SeekFrom::Start(next))
            .map_err(|e| format!("seek: {e}"))?;
    }

    if !found_anim || frame_count == 0 {
        return Ok(None);
    }

    Ok(Some(WebpInfo {
        width,
        height,
        frame_count,
        total_duration_ms,
        loop_count,
        has_alpha,
    }))
}

/// Probe an animated WebP file for canvas dimensions, frame count, and
/// total duration. Returns `None` if the file is not an animated WebP.
pub fn probe_webp(path: &Path) -> Result<Option<WebpInfo>, String> {
    probe_metadata(path)
}

/// Extract all animation frames from an animated WebP file.
/// Each frame's compressed data is wrapped as a minimal standalone WebP
/// file so ffmpeg's static WebP decoder can handle it.
fn extract_frames(path: &Path) -> Result<(WebpInfo, Vec<AnimFrame>), String> {
    let mut file =
        std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;

    // Skip RIFF header (12 bytes: "RIFF" + size + "WEBP")
    file.seek(SeekFrom::Start(12))
        .map_err(|e| format!("seek: {e}"))?;

    let mut width: u32 = 0;
    let mut height: u32 = 0;
    let mut has_alpha = false;
    let mut loop_count: u16 = 0;
    let mut total_duration_ms: u32 = 0;
    let mut frames: Vec<AnimFrame> = Vec::new();

    while let Ok(chunk_id) = read_fourcc(&mut file) {
        let chunk_size = match read_u32_le(&mut file) {
            Ok(s) => s,
            Err(_) => break,
        };
        let chunk_start = file.stream_position().map_err(|e| format!("{e}"))?;

        match &chunk_id {
            b"VP8X" => {
                let mut flags = [0u8; 4];
                file.read_exact(&mut flags)
                    .map_err(|e| format!("VP8X: {e}"))?;
                has_alpha = (flags[0] & 0x10) != 0;
                width = read_u24_le(&mut file)? + 1;
                height = read_u24_le(&mut file)? + 1;
            }
            b"ANIM" => {
                let _bg = read_u32_le(&mut file)?;
                loop_count = read_u16_le(&mut file)?;
            }
            b"ANMF" => {
                let x = read_u24_le(&mut file)? * 2; // spec: in units of 2 pixels
                let y = read_u24_le(&mut file)? * 2;
                let w = read_u24_le(&mut file)? + 1;
                let h = read_u24_le(&mut file)? + 1;
                let dur = read_u24_le(&mut file)?;
                let mut flags_byte = [0u8; 1];
                file.read_exact(&mut flags_byte)
                    .map_err(|e| format!("ANMF flags: {e}"))?;
                let dispose = (flags_byte[0] & 0x01) != 0;
                let blend = (flags_byte[0] & 0x02) == 0; // 0 = blend, 1 = no blend

                // Read the compressed frame data (rest of the ANMF chunk)
                let header_bytes = 3 + 3 + 3 + 3 + 3 + 1; // 16 bytes of ANMF header
                if (chunk_size as usize) < header_bytes {
                    return Err(format!("ANMF chunk too small: {} bytes", chunk_size));
                }
                let data_size = chunk_size as usize - header_bytes;
                const MAX_CHUNK_SIZE: usize = 256 * 1024 * 1024;
                if data_size > MAX_CHUNK_SIZE {
                    return Err(format!(
                        "ANMF chunk too large: {} bytes (max {})",
                        data_size, MAX_CHUNK_SIZE,
                    ));
                }
                let mut data = vec![0u8; data_size];
                file.read_exact(&mut data)
                    .map_err(|e| format!("ANMF data: {e}"))?;

                total_duration_ms = total_duration_ms.saturating_add(dur);
                frames.push(AnimFrame {
                    x_offset: x,
                    y_offset: y,
                    width: w,
                    height: h,
                    duration_ms: dur,
                    dispose,
                    blend,
                    data,
                });
            }
            _ => {}
        }

        let padded_size = chunk_size as u64 + (chunk_size as u64 & 1);
        let next = chunk_start + padded_size;
        file.seek(SeekFrom::Start(next))
            .map_err(|e| format!("seek: {e}"))?;
    }

    let info = WebpInfo {
        width,
        height,
        frame_count: frames.len() as u32,
        total_duration_ms,
        loop_count,
        has_alpha,
    };

    Ok((info, frames))
}

/// Wrap a raw frame bitstream as a minimal standalone WebP file.
/// ffmpeg's static WebP decoder needs the RIFF/WEBP header to recognise
/// the input format.
fn wrap_as_standalone_webp(frame_data: &[u8]) -> Vec<u8> {
    // Detect the sub-format from the frame data's leading bytes
    let is_vp8l = frame_data.starts_with(b"VP8L");
    let is_vp8 = frame_data.starts_with(b"VP8 ");
    let is_alph = frame_data.starts_with(b"ALPH");

    if is_vp8 || is_vp8l || is_alph {
        // Frame data already has chunk headers - wrap with RIFF/WEBP
        let file_size = 4 + frame_data.len(); // "WEBP" + data
        let mut buf = Vec::with_capacity(12 + frame_data.len());
        buf.extend_from_slice(b"RIFF");
        buf.extend_from_slice(&(file_size as u32).to_le_bytes());
        buf.extend_from_slice(b"WEBP");
        buf.extend_from_slice(frame_data);
        buf
    } else {
        // Raw VP8 bitstream without chunk header - wrap as "VP8 " chunk
        let chunk_size = frame_data.len() as u32;
        let file_size = 4 + 8 + frame_data.len(); // "WEBP" + "VP8 " + size + data
        let mut buf = Vec::with_capacity(12 + 8 + frame_data.len());
        buf.extend_from_slice(b"RIFF");
        buf.extend_from_slice(&(file_size as u32).to_le_bytes());
        buf.extend_from_slice(b"WEBP");
        buf.extend_from_slice(b"VP8 ");
        buf.extend_from_slice(&chunk_size.to_le_bytes());
        buf.extend_from_slice(frame_data);
        buf
    }
}

// ── Canvas compositing ─────────────────────────────────────────

/// RGBA pixel canvas for frame compositing.
struct Canvas {
    width: u32,
    height: u32,
    /// Row-major RGBA pixels, length = width * height * 4.
    pixels: Vec<u8>,
}

impl Canvas {
    fn new(width: u32, height: u32) -> Result<Self, String> {
        let size = (width as usize)
            .checked_mul(height as usize)
            .and_then(|v| v.checked_mul(4))
            .ok_or_else(|| format!("Canvas dimensions too large: {}x{}", width, height))?;
        Ok(Self {
            width,
            height,
            pixels: vec![0u8; size],
        })
    }

    /// Clear the entire canvas to transparent black.
    fn clear(&mut self) {
        self.pixels.fill(0);
    }

    /// Clear a rectangular region to transparent black.
    fn clear_rect(&mut self, x: u32, y: u32, w: u32, h: u32) {
        if x >= self.width || y >= self.height || w == 0 || h == 0 {
            return;
        }
        let y_end = (y + h).min(self.height);
        let x_end = (x + w).min(self.width);
        for row in y..y_end {
            let start = ((row * self.width + x) * 4) as usize;
            let end = ((row * self.width + x_end) * 4) as usize;
            if start < self.pixels.len() && end <= self.pixels.len() {
                self.pixels[start..end].fill(0);
            }
        }
    }

    /// Composite decoded RGBA pixels onto the canvas at the given offset.
    /// If `blend` is true, alpha-blend; otherwise overwrite.
    fn composite(&mut self, frame_rgba: &[u8], x: u32, y: u32, w: u32, h: u32, blend: bool) {
        for row in 0..h {
            let canvas_y = y + row;
            if canvas_y >= self.height {
                break;
            }
            for col in 0..w {
                let canvas_x = x + col;
                if canvas_x >= self.width {
                    break;
                }
                let src_idx = ((row * w + col) * 4) as usize;
                let dst_idx = ((canvas_y * self.width + canvas_x) * 4) as usize;

                if src_idx + 3 >= frame_rgba.len() || dst_idx + 3 >= self.pixels.len() {
                    continue;
                }

                if blend {
                    // Alpha compositing: src over dst
                    let sa = frame_rgba[src_idx + 3] as u32;
                    if sa == 255 {
                        self.pixels[dst_idx..dst_idx + 4]
                            .copy_from_slice(&frame_rgba[src_idx..src_idx + 4]);
                    } else if sa > 0 {
                        let da = self.pixels[dst_idx + 3] as u32;
                        let inv_sa = 255 - sa;
                        for c in 0..3 {
                            let sc = frame_rgba[src_idx + c] as u32;
                            let dc = self.pixels[dst_idx + c] as u32;
                            self.pixels[dst_idx + c] = ((sc * sa + dc * da * inv_sa / 255)
                                / (sa + da * inv_sa / 255).max(1))
                                as u8;
                        }
                        self.pixels[dst_idx + 3] = (sa + da * inv_sa / 255) as u8;
                    }
                    // sa == 0: source pixel fully transparent, leave dst unchanged
                } else {
                    // Overwrite mode
                    self.pixels[dst_idx..dst_idx + 4]
                        .copy_from_slice(&frame_rgba[src_idx..src_idx + 4]);
                }
            }
        }
    }

    /// Return the canvas pixels as a contiguous RGBA byte slice.
    fn as_rgba(&self) -> &[u8] {
        &self.pixels
    }
}

// ── Decode + encode pipeline ───────────────────────────────────

/// Inputs for the encoder command of the animated WebP pipeline.
pub(crate) struct WebpEncodeArgs<'a> {
    pub video_args: &'a [String],
    pub threads: u32,
    /// Padded (even) canvas size of the piped RGBA frames.
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub output_path: &'a str,
    /// Device and upload arguments for the encoder (`hw_device_args`).
    pub hw: &'a crate::encoder::HwArgs,
    /// `hevc_vaapi_size_fix` for this encode.
    pub size_fix: Option<&'a crate::encoder::HevcVaapiSizeFix>,
}

/// ffmpeg arguments for the encoder that reads RGBA frames from stdin.
/// Hardware encoders get the same device and upload arguments as the
/// main encode path.
pub(crate) fn webp_encoder_args(a: &WebpEncodeArgs<'_>) -> Vec<String> {
    let mut args: Vec<String> = vec!["-y".into()];
    args.extend(a.hw.pre_input.iter().cloned());
    args.extend([
        "-f".into(),
        "rawvideo".into(),
        "-pix_fmt".into(),
        "rgba".into(),
        "-s".into(),
        format!("{}x{}", a.width, a.height),
        "-r".into(),
        format!("{:.0}", a.fps),
        "-i".into(),
        "pipe:0".into(),
        "-an".into(),
        "-sn".into(),
    ]);
    if a.threads > 0 {
        args.push("-threads".into());
        args.push(a.threads.to_string());
    }
    args.extend(a.video_args.iter().cloned());
    let pad = a.size_fix.and_then(|f| f.pad.as_deref());
    let vf = match (pad, &a.hw.upload_filter) {
        (Some(pad), Some(up)) => Some(format!("{pad},{up}")),
        (Some(pad), None) => Some(pad.to_string()),
        (None, up) => up.clone(),
    };
    if let Some(vf) = vf {
        args.push("-vf".into());
        args.push(vf);
    }
    if a.hw.emit_pix_fmt {
        args.push("-pix_fmt".into());
        args.push("yuv420p".into());
    }
    if let Some(fix) = a.size_fix {
        args.push("-bsf:v".into());
        args.push(fix.bsf.clone());
    }
    if a.output_path.ends_with(".mp4") {
        args.push("-movflags".into());
        args.push("+faststart".into());
    }
    args.push(a.output_path.to_string());
    args
}

/// Base framerate for the output. Individual frame durations are
/// honoured by writing each frame multiple times proportional to its
/// duration (frame duplication). 50fps = 20ms per tick, which is the
/// minimum frame duration in the WebP spec. The encoder compresses
/// duplicate frames to near-zero cost (identical P-frames).
const BASE_FPS: f64 = 50.0;

/// Canvas size padded to even dimensions (required by most encoders).
fn padded_canvas(info: &WebpInfo) -> (u32, u32) {
    ((info.width + 1) & !1, (info.height + 1) & !1)
}

/// ffmpeg arguments for the decoder that reads standalone WebP frames
/// from stdin and writes raw RGBA to stdout.
fn webp_decoder_args() -> Vec<String> {
    [
        "-f",
        "webp_pipe",
        "-i",
        "pipe:0",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgba",
        "-v",
        "error",
        "pipe:1",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Decode an animated WebP file and encode it to a video file.
///
/// Uses two ffmpeg processes:
/// 1. A decoder that receives individual WebP frames via image2pipe stdin
///    and outputs raw RGBA pixels to stdout.
/// 2. An encoder that receives composited raw RGBA frames via rawvideo
///    stdin and writes the output file.
///
/// Progress is reported per-frame via `sink.file_progress`.
pub async fn transcode_animated_webp(
    input_path: &str,
    output_path: &str,
    video_args: &[String],
    threads: u32,
    low_priority: bool,
    sink: &dyn EventSink,
    batch_control: &dyn BatchControl,
) -> Result<TranscodeResult, String> {
    let path = Path::new(input_path);

    // ── Extract frames from the container ──
    sink.log("  Animated WebP: parsing RIFF container...");
    let (info, frames) = extract_frames(path)?;

    if frames.is_empty() {
        return Err("No animation frames found in WebP file".into());
    }

    // Check if frame durations vary
    let has_variable_timing = if frames.len() > 1 {
        let first_dur = frames[0].duration_ms;
        frames.iter().any(|f| f.duration_ms != first_dur)
    } else {
        false
    };
    let timing_label = if has_variable_timing {
        "variable timing"
    } else {
        "constant timing"
    };
    sink.log(&format!(
        "  Animated WebP: {}x{}, {} frames, {:.1}s ({})",
        info.width,
        info.height,
        info.frame_count,
        info.total_duration_ms as f64 / 1000.0,
        timing_label,
    ));

    let (padded_w, padded_h) = padded_canvas(&info);

    // ── Spawn the output encoder ──
    let encoder = video_args
        .windows(2)
        .find(|w| w[0] == "-c:v")
        .map(|w| w[1].as_str())
        .unwrap_or("");
    let hw = crate::encoder::hw_device_args(
        encoder,
        "yuv420p",
        false,
        crate::encoder::render_node_for(encoder).as_deref(),
        crate::encoder::TargetOs::current(),
    );
    let size_fix = crate::encoder::hevc_vaapi_size_fix(
        encoder,
        padded_w,
        padded_h,
        crate::encoder::ffmpeg_caps().hevc_metadata_size,
    );
    let enc_args = webp_encoder_args(&WebpEncodeArgs {
        video_args,
        threads,
        width: padded_w,
        height: padded_h,
        fps: BASE_FPS,
        output_path,
        hw: &hw,
        size_fix: size_fix.as_ref(),
    });

    let mut enc_cmd = crate::ffmpeg::ffmpeg_command();
    enc_cmd.args(&enc_args);

    if low_priority {
        #[cfg(target_os = "windows")]
        {
            const BELOW_NORMAL: u32 = 0x00004000;
            const NO_WINDOW: u32 = 0x08000000;
            enc_cmd.creation_flags(BELOW_NORMAL | NO_WINDOW);
        }
    }

    let mut dec_cmd = crate::ffmpeg::ffmpeg_command();
    dec_cmd.args(webp_decoder_args());

    run_pipeline(
        &frames,
        &info,
        enc_cmd,
        dec_cmd,
        low_priority,
        sink,
        batch_control,
    )
    .await
}

/// Run the decoder and encoder children of the animated WebP pipeline.
/// The commands carry their arguments; this sets their stdio.
async fn run_pipeline(
    frames: &[AnimFrame],
    info: &WebpInfo,
    mut enc_cmd: tokio::process::Command,
    mut dec_cmd: tokio::process::Command,
    low_priority: bool,
    sink: &dyn EventSink,
    batch_control: &dyn BatchControl,
) -> Result<TranscodeResult, String> {
    let ms_per_tick: f64 = 1000.0 / BASE_FPS;
    let (padded_w, padded_h) = padded_canvas(info);
    let frame_byte_size = (padded_w as usize)
        .checked_mul(padded_h as usize)
        .and_then(|v| v.checked_mul(4))
        .ok_or_else(|| {
            format!(
                "Padded canvas dimensions too large: {}x{}",
                padded_w, padded_h
            )
        })?;

    // Allocate before spawning so an error here cannot leave children behind.
    let mut canvas = Canvas::new(padded_w, padded_h)?;
    canvas.clear(); // WebP spec: canvas starts as transparent black

    enc_cmd
        .kill_on_drop(true)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());

    let mut encoder = enc_cmd
        .spawn()
        .map_err(|e| format!("Failed to launch encoder ffmpeg: {e}"))?;

    #[cfg(unix)]
    if low_priority {
        if let Some(pid) = encoder.id() {
            let _ = std::process::Command::new("renice")
                .args(["-n", "10", "-p", &pid.to_string()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }

    sink.log(&format!(
        "  Encoder ffmpeg PID: {}",
        encoder.id().unwrap_or(0)
    ));

    let mut enc_stdin = encoder
        .stdin
        .take()
        .ok_or_else(|| "Could not open encoder stdin".to_string())?;
    // An undrained stderr pipe fills over a long encode and blocks ffmpeg.
    let enc_stderr = drain_stderr(encoder.stderr.take());

    // ── Spawn a single decoder process for all frames ──
    // Instead of spawning one ffmpeg process per frame, we use a single
    // long-lived process with webp_pipe input. We write each frame's
    // standalone WebP data to its stdin and read the decoded RGBA pixels
    // from its stdout. A background task feeds stdin while the main loop
    // reads stdout, avoiding pipe deadlocks.
    dec_cmd
        .kill_on_drop(true)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut decoder = dec_cmd
        .spawn()
        .map_err(|e| format!("Failed to launch decoder ffmpeg: {e}"))?;

    let dec_stdin = decoder
        .stdin
        .take()
        .ok_or_else(|| "Could not open decoder stdin".to_string())?;
    let mut dec_stdout = decoder
        .stdout
        .take()
        .ok_or_else(|| "Could not open decoder stdout".to_string())?;
    let dec_stderr = drain_stderr(decoder.stderr.take());

    // Prepare the frame payloads for the writer task. Each entry is the
    // standalone-wrapped WebP data to write to the decoder's stdin.
    let frame_payloads: Vec<Vec<u8>> = frames
        .iter()
        .map(|f| wrap_as_standalone_webp(&f.data))
        .collect();

    // Spawn a background task that feeds all frames to the decoder's stdin.
    // This runs concurrently with the stdout reader to avoid deadlock.
    let mut writer_handle = tokio::spawn(async move {
        let mut stdin = dec_stdin;
        for payload in &frame_payloads {
            if stdin.write_all(payload).await.is_err() {
                break;
            }
        }
        // Dropping stdin sends EOF so the decoder knows we are done.
        drop(stdin);
    });

    // ── Compositing loop ──
    let total_frames = frames.len();
    let mut decoded_count: u32 = 0;
    // Set when the loop stops before the last frame.
    let mut stop: Option<Stop> = None;

    'frames: for (i, frame) in frames.iter().enumerate() {
        // Check for cancellation
        if cancel_requested(batch_control) {
            stop = Some(Stop::Cancelled);
            break;
        }

        // Read decoded RGBA pixels for this frame from the single decoder
        // process. Each frame produces exactly width * height * 4 bytes.
        let Some(expected_size) = (frame.width as usize)
            .checked_mul(frame.height as usize)
            .and_then(|v| v.checked_mul(4))
        else {
            stop = Some(Stop::Failed(format!(
                "Frame {} dimensions too large: {}x{}",
                i + 1,
                frame.width,
                frame.height
            )));
            break;
        };
        let mut decoded_rgba = vec![0u8; expected_size];
        match until_cancelled(read_full(&mut dec_stdout, &mut decoded_rgba), batch_control).await {
            None => {
                stop = Some(Stop::Cancelled);
                break;
            }
            Some(Err(e)) => {
                stop = Some(Stop::Failed(format!(
                    "Frame {} decode read failed: {e}",
                    i + 1
                )));
                break;
            }
            // A short read leaves the zero-filled remainder in place.
            Some(Ok(_)) => {}
        }

        // Composite onto canvas
        if !frame.blend {
            // Overwrite mode: clear the frame region first
            canvas.clear_rect(frame.x_offset, frame.y_offset, frame.width, frame.height);
        }
        canvas.composite(
            &decoded_rgba,
            frame.x_offset,
            frame.y_offset,
            frame.width,
            frame.height,
            frame.blend,
        );

        // Write the composited canvas to the encoder's stdin.
        // Each frame is written multiple times to honour its duration at
        // the base framerate. A 100ms frame at 50fps = 5 writes.
        let repeat_count = ((frame.duration_ms as f64 / ms_per_tick).round() as u32).max(1);
        let canvas_bytes = canvas.as_rgba();
        let write_bytes: &[u8] = if canvas_bytes.len() == frame_byte_size {
            canvas_bytes
        } else {
            // This shouldn't happen with correct padding, but handle it safely
            &canvas_bytes[..canvas_bytes.len().min(frame_byte_size)]
        };
        for _ in 0..repeat_count {
            match until_cancelled(enc_stdin.write_all(write_bytes), batch_control).await {
                None => {
                    stop = Some(Stop::Cancelled);
                    break 'frames;
                }
                Some(Err(e)) => {
                    sink.log(&format!(
                        "  WARNING: Encoder pipe closed at frame {}: {e}",
                        i + 1
                    ));
                    stop = Some(Stop::EncoderClosed);
                    break 'frames;
                }
                Some(Ok(())) => {}
            }
        }

        // Disposal for the *current* frame (applies to the next frame's rendering)
        if frame.dispose {
            canvas.clear_rect(frame.x_offset, frame.y_offset, frame.width, frame.height);
        }

        decoded_count += 1;

        // Report progress
        let pct = (decoded_count as f64 / total_frames as f64) * 100.0;
        let elapsed_secs =
            (info.total_duration_ms as f64 / 1000.0) * (decoded_count as f64 / total_frames as f64);
        let total_secs = info.total_duration_ms as f64 / 1000.0;
        sink.file_progress(pct, elapsed_secs, total_secs, None);
    }

    // ── Teardown ──
    // Close encoder stdin to signal EOF
    drop(enc_stdin);

    // Never await the feeder while the decoder can still block it: a
    // decoder whose stdout nobody reads stops reading its stdin, and the
    // feeder then waits for ever. Stop the decoder first if the loop ended
    // early, and close its stdout in every case.
    if stop.is_some() {
        let _ = decoder.start_kill();
    }
    if matches!(stop, Some(Stop::Cancelled | Stop::Failed(_))) {
        let _ = encoder.start_kill();
    }
    drop(dec_stdout);

    // Each wait below gives way to a cancel; the kill then lets it finish.
    if until_cancelled(&mut writer_handle, batch_control)
        .await
        .is_none()
    {
        stop = Some(Stop::Cancelled);
        let _ = decoder.start_kill();
        let _ = encoder.start_kill();
        let _ = writer_handle.await;
    }

    if until_cancelled(decoder.wait(), batch_control)
        .await
        .is_none()
    {
        stop = Some(Stop::Cancelled);
        let _ = decoder.start_kill();
        let _ = encoder.start_kill();
    }
    let _ = decoder.wait().await;

    // Wait for encoder to finish
    if until_cancelled(encoder.wait(), batch_control)
        .await
        .is_none()
    {
        stop = Some(Stop::Cancelled);
        let _ = encoder.start_kill();
    }
    let enc_status = encoder
        .wait()
        .await
        .map_err(|e| format!("Encoder wait failed: {e}"))?;
    // Both children have exited, so their stderr pipes are at EOF.
    let enc_tail = enc_stderr.await.unwrap_or_default();
    let _ = dec_stderr.await;

    let exit_code = enc_status.code().unwrap_or(-1);
    if exit_code != 0 && !matches!(stop, Some(Stop::Cancelled)) {
        if let Some(line) = last_line(&enc_tail) {
            sink.log(&format!("  Encoder ffmpeg: {line}"));
        }
    }
    if let Some(Stop::Failed(msg)) = stop {
        return Err(msg);
    }
    Ok(TranscodeResult {
        exit_code,
        was_cancelled: matches!(stop, Some(Stop::Cancelled)),
        frame_count: decoded_count as u64,
    })
}

/// Why the frame loop of `run_pipeline` stopped early.
enum Stop {
    Cancelled,
    /// The encoder closed its input; its exit code decides the result.
    EncoderClosed,
    Failed(String),
}

/// Bytes of a child's stderr kept for error messages.
const STDERR_TAIL: usize = 4096;

/// Read a child's stderr to EOF so the child never blocks on a full pipe.
/// Resolves to the last `STDERR_TAIL` bytes.
fn drain_stderr(stderr: Option<tokio::process::ChildStderr>) -> tokio::task::JoinHandle<Vec<u8>> {
    tokio::spawn(async move {
        let mut tail = Vec::new();
        let Some(mut stderr) = stderr else {
            return tail;
        };
        let mut buf = [0u8; 4096];
        while let Ok(n) = stderr.read(&mut buf).await {
            if n == 0 {
                break;
            }
            tail.extend_from_slice(&buf[..n]);
            if tail.len() > 2 * STDERR_TAIL {
                tail.drain(..tail.len() - STDERR_TAIL);
            }
        }
        if tail.len() > STDERR_TAIL {
            tail.drain(..tail.len() - STDERR_TAIL);
        }
        tail
    })
}

/// The last non-empty line of a stderr tail (ffmpeg ends progress lines
/// with `\r`).
fn last_line(tail: &[u8]) -> Option<String> {
    String::from_utf8_lossy(tail)
        .split(['\n', '\r'])
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .map(str::to_string)
}

fn cancel_requested(control: &dyn BatchControl) -> bool {
    control.should_cancel_current() || control.should_cancel_all()
}

/// Await `fut`, checking for a cancel every 100 ms. Returns `None` if the
/// user cancels first, so a stalled child cannot make cancel wait.
async fn until_cancelled<F: std::future::Future>(
    fut: F,
    control: &dyn BatchControl,
) -> Option<F::Output> {
    tokio::pin!(fut);
    loop {
        if cancel_requested(control) {
            return None;
        }
        tokio::select! {
            out = &mut fut => return Some(out),
            _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
        }
    }
}

/// Read until `buf` is full or the reader reaches EOF; returns the bytes read.
async fn read_full(
    r: &mut (impl tokio::io::AsyncRead + Unpin),
    buf: &mut [u8],
) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]).await? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// Result from the animated WebP transcode pipeline.
pub struct TranscodeResult {
    pub exit_code: i32,
    pub was_cancelled: bool,
    pub frame_count: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoder::{hw_device_args, TargetOs};

    fn enc_args(enc: &str, fix: Option<&crate::encoder::HevcVaapiSizeFix>) -> Vec<String> {
        let video_args: Vec<String> = ["-c:v", enc].iter().map(|s| s.to_string()).collect();
        let hw = hw_device_args(
            enc,
            "yuv420p",
            false,
            Some("/dev/dri/renderD128"),
            TargetOs::Linux,
        );
        webp_encoder_args(&WebpEncodeArgs {
            video_args: &video_args,
            threads: 0,
            width: 322,
            height: 242,
            fps: 50.0,
            output_path: "/o/out.mkv",
            hw: &hw,
            size_fix: fix,
        })
    }

    #[test]
    fn webp_encoder_args_upload_for_vaapi() {
        let fix = crate::encoder::hevc_vaapi_size_fix("hevc_vaapi", 322, 242, true);
        let a = enc_args("hevc_vaapi", fix.as_ref());
        let dev = a.iter().position(|x| x == "-vaapi_device").unwrap();
        assert!(dev < a.iter().position(|x| x == "-f").unwrap());
        assert!(a
            .windows(2)
            .any(|w| w[0] == "-vf" && w[1] == "format=nv12,hwupload"));
        assert!(a.windows(2).any(|w| w[0] == "-s" && w[1] == "322x242"));
        assert!(a
            .windows(2)
            .any(|w| w[0] == "-bsf:v" && w[1] == "hevc_metadata=width=322:height=242"));
        assert_eq!(a.last().unwrap(), "/o/out.mkv");
    }

    /// Older ffmpeg: the pad runs before the upload.
    #[test]
    fn webp_encoder_args_vaapi_pad_fallback() {
        let fix = crate::encoder::hevc_vaapi_size_fix("hevc_vaapi", 322, 242, false);
        let a = enc_args("hevc_vaapi", fix.as_ref());
        assert!(a
            .windows(2)
            .any(|w| w[0] == "-vf" && w[1] == "pad=384:256,format=nv12,hwupload"));
        assert!(a
            .windows(2)
            .any(|w| w[0] == "-bsf:v" && w[1] == "hevc_metadata=crop_right=62:crop_bottom=14"));
    }

    #[test]
    fn webp_encoder_args_software_unchanged() {
        let a = enc_args("libx265", None);
        let want: Vec<String> = [
            "-y",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "-s",
            "322x242",
            "-r",
            "50",
            "-i",
            "pipe:0",
            "-an",
            "-sn",
            "-c:v",
            "libx265",
            "-pix_fmt",
            "yuv420p",
            "/o/out.mkv",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(a, want);
    }

    #[test]
    fn webp_encoder_args_vulkan_no_pix_fmt() {
        let a = enc_args("hevc_vulkan", None);
        assert!(a.iter().any(|x| x == "-init_hw_device"));
        assert!(a
            .windows(2)
            .any(|w| w[0] == "-vf" && w[1] == "format=nv12,hwupload"));
        assert_eq!(
            a.iter().filter(|x| *x == "-pix_fmt").count(),
            1,
            "only the rgba input"
        );
    }

    #[test]
    fn test_canvas_new_overflow_returns_err() {
        // u32::MAX * u32::MAX * 4 overflows usize — must return Err.
        let result = Canvas::new(u32::MAX, u32::MAX);
        assert!(result.is_err());
    }

    #[test]
    fn test_canvas_new_valid_dimensions() {
        let result = Canvas::new(100, 100);
        assert!(result.is_ok());
        let canvas = result.unwrap();
        assert_eq!(canvas.width, 100);
        assert_eq!(canvas.height, 100);
        assert_eq!(canvas.pixels.len(), 100 * 100 * 4);
    }

    #[test]
    fn test_canvas_new_zero_dimension() {
        // Zero width or height produces a zero-length pixel buffer.
        let result = Canvas::new(0, 100);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().pixels.len(), 0);
    }

    #[test]
    fn test_canvas_clear() {
        let mut canvas = Canvas::new(4, 4).unwrap();
        canvas.pixels.fill(0xFF);
        canvas.clear();
        assert!(canvas.pixels.iter().all(|&b| b == 0));
    }

    #[test]
    fn test_canvas_clear_rect() {
        let mut canvas = Canvas::new(4, 4).unwrap();
        canvas.pixels.fill(0xFF);
        canvas.clear_rect(1, 1, 2, 2);

        for row in 0..4u32 {
            for col in 0..4u32 {
                let idx = ((row * 4 + col) * 4) as usize;
                let pixel = &canvas.pixels[idx..idx + 4];
                if (1..3).contains(&row) && (1..3).contains(&col) {
                    assert!(
                        pixel.iter().all(|&b| b == 0),
                        "pixel ({col},{row}) should be zeroed"
                    );
                } else {
                    assert!(
                        pixel.iter().all(|&b| b == 0xFF),
                        "pixel ({col},{row}) should still be 0xFF"
                    );
                }
            }
        }
    }

    #[test]
    fn test_canvas_composite_overwrite() {
        let mut canvas = Canvas::new(4, 4).unwrap();
        // Canvas starts all zeros.
        // Create a 2x2 frame of all 0xFF (including alpha=255).
        let frame = vec![0xFFu8; 2 * 2 * 4];
        canvas.composite(&frame, 1, 1, 2, 2, false);

        for row in 0..4u32 {
            for col in 0..4u32 {
                let idx = ((row * 4 + col) * 4) as usize;
                let pixel = &canvas.pixels[idx..idx + 4];
                if (1..3).contains(&row) && (1..3).contains(&col) {
                    assert!(
                        pixel.iter().all(|&b| b == 0xFF),
                        "pixel ({col},{row}) should be 0xFF"
                    );
                } else {
                    assert!(
                        pixel.iter().all(|&b| b == 0),
                        "pixel ({col},{row}) should be 0"
                    );
                }
            }
        }
    }

    #[test]
    fn test_canvas_composite_blend_opaque() {
        let mut canvas = Canvas::new(4, 4).unwrap();
        // Fill background with known color (50, 100, 150, 255).
        for pixel in canvas.pixels.chunks_exact_mut(4) {
            pixel.copy_from_slice(&[50, 100, 150, 255]);
        }
        // Composite a fully opaque 2x2 frame (200, 200, 200, 255).
        let frame: Vec<u8> = [200u8, 200, 200, 255].repeat(2 * 2);
        canvas.composite(&frame, 1, 1, 2, 2, true);

        for row in 0..4u32 {
            for col in 0..4u32 {
                let idx = ((row * 4 + col) * 4) as usize;
                let pixel = &canvas.pixels[idx..idx + 4];
                if (1..3).contains(&row) && (1..3).contains(&col) {
                    assert_eq!(
                        pixel,
                        &[200, 200, 200, 255],
                        "pixel ({col},{row}) should be overwritten by opaque source"
                    );
                } else {
                    assert_eq!(
                        pixel,
                        &[50, 100, 150, 255],
                        "pixel ({col},{row}) should be unchanged"
                    );
                }
            }
        }
    }

    #[test]
    fn test_canvas_composite_blend_transparent() {
        let mut canvas = Canvas::new(4, 4).unwrap();
        // Fill background with known color.
        for pixel in canvas.pixels.chunks_exact_mut(4) {
            pixel.copy_from_slice(&[80, 120, 160, 255]);
        }
        // Composite a fully transparent 2x2 frame (alpha=0).
        let frame: Vec<u8> = [200u8, 200, 200, 0].repeat(2 * 2);
        canvas.composite(&frame, 1, 1, 2, 2, true);

        // Destination should be completely unchanged.
        for pixel in canvas.pixels.chunks_exact(4) {
            assert_eq!(pixel, &[80, 120, 160, 255]);
        }
    }

    #[test]
    fn test_canvas_composite_boundary_clip() {
        let mut canvas = Canvas::new(4, 4).unwrap();
        // Composite a 3x3 frame at offset (3,3) — only pixel (3,3) overlaps.
        let frame: Vec<u8> = vec![0xFF; 3 * 3 * 4];
        canvas.composite(&frame, 3, 3, 3, 3, false);

        for row in 0..4u32 {
            for col in 0..4u32 {
                let idx = ((row * 4 + col) * 4) as usize;
                let pixel = &canvas.pixels[idx..idx + 4];
                if row == 3 && col == 3 {
                    assert!(
                        pixel.iter().all(|&b| b == 0xFF),
                        "pixel (3,3) should be written"
                    );
                } else {
                    assert!(
                        pixel.iter().all(|&b| b == 0),
                        "pixel ({col},{row}) should be unchanged"
                    );
                }
            }
        }
    }

    #[test]
    fn test_canvas_composite_partial_alpha() {
        let mut canvas = Canvas::new(4, 4).unwrap();
        // Fill background with (100, 100, 100, 255).
        for pixel in canvas.pixels.chunks_exact_mut(4) {
            pixel.copy_from_slice(&[100, 100, 100, 255]);
        }
        // Composite 1x1 pixel with (200, 200, 200, 128) at (0,0) with blend.
        let frame = vec![200u8, 200, 200, 128];
        canvas.composite(&frame, 0, 0, 1, 1, true);

        let pixel = &canvas.pixels[0..4];
        // The blending formula in composite:
        //   sa=128, da=255, inv_sa=127
        //   out_c = (sc*sa + dc*da*inv_sa/255) / max(sa + da*inv_sa/255, 1)
        //         = (200*128 + 100*255*127/255) / max(128 + 255*127/255, 1)
        //         = (25600 + 12700) / (128 + 127) = 38300/255 ≈ 150
        //   out_a = sa + da*inv_sa/255 = 128 + 127 = 255
        // Allow tolerance of ±2 for integer rounding.
        assert!(
            (pixel[0] as i32 - 150).abs() <= 2,
            "red channel: expected ~150, got {}",
            pixel[0]
        );
        assert!(
            (pixel[1] as i32 - 150).abs() <= 2,
            "green channel: expected ~150, got {}",
            pixel[1]
        );
        assert!(
            (pixel[2] as i32 - 150).abs() <= 2,
            "blue channel: expected ~150, got {}",
            pixel[2]
        );
        assert_eq!(pixel[3], 255, "alpha should be 255");
    }

    #[test]
    fn last_line_skips_progress_and_blank_lines() {
        let tail = b"frame=  10 fps=0.0\rframe=  20 fps=9.9\r\nError while encoding\n\n";
        assert_eq!(last_line(tail).as_deref(), Some("Error while encoding"));
        assert_eq!(last_line(b"\r\n  \n"), None);
    }

    // ── Pipeline supervision (stub children) ─────────────────────

    /// Drives `run_pipeline` with `sh -c` stubs in place of ffmpeg.
    #[cfg(unix)]
    mod pipeline {
        use super::*;
        use crate::test_helpers::{CancellableBatchControl, NoopSink};
        use std::sync::Arc;
        use std::time::Duration;

        const W: u32 = 64;
        const H: u32 = 48;
        const FRAME_BYTES: usize = (W * H * 4) as usize;
        const FRAMES: usize = 4;
        /// Big enough that the feeder cannot finish into the pipe buffers
        /// alone: 4 x 256 KiB against 64 KiB pipes.
        const PAYLOAD: usize = 256 * 1024;
        const WATCHDOG: Duration = Duration::from_secs(20);

        /// Scripts `exec` their last command so a kill reaches the process
        /// that holds the pipes, as it does for ffmpeg.
        fn sh(script: &str) -> tokio::process::Command {
            let mut c = tokio::process::Command::new("sh");
            c.arg("-c").arg(script).kill_on_drop(true);
            c
        }

        fn synthetic() -> (WebpInfo, Vec<AnimFrame>) {
            let frames: Vec<AnimFrame> = (0..FRAMES)
                .map(|_| AnimFrame {
                    x_offset: 0,
                    y_offset: 0,
                    width: W,
                    height: H,
                    duration_ms: 100,
                    dispose: false,
                    blend: false,
                    data: vec![0x55; PAYLOAD],
                })
                .collect();
            let info = WebpInfo {
                width: W,
                height: H,
                frame_count: FRAMES as u32,
                total_duration_ms: 100 * FRAMES as u32,
                loop_count: 0,
                has_alpha: false,
            };
            (info, frames)
        }

        /// Run the pipeline on its own thread and runtime; fail the test
        /// instead of hanging the suite if it does not return.
        fn run_watched(
            enc: tokio::process::Command,
            dec: tokio::process::Command,
            control: Arc<CancellableBatchControl>,
        ) -> Result<TranscodeResult, String> {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let (info, frames) = synthetic();
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let r = rt.block_on(run_pipeline(
                    &frames, &info, enc, dec, false, &NoopSink, &*control,
                ));
                let _ = tx.send(r);
            });
            rx.recv_timeout(WATCHDOG)
                .unwrap_or_else(|_| panic!("pipeline did not return within {WATCHDOG:?}"))
        }

        /// Decoder stub: swallow all input, then emit `n` frames of zeros.
        fn decoder_emitting(n: usize) -> tokio::process::Command {
            sh(&format!(
                "cat >/dev/null; exec head -c {} /dev/zero",
                n * FRAME_BYTES
            ))
        }

        /// Control: healthy stubs complete, so the harness can pass.
        #[test]
        fn healthy_stubs_complete() {
            let r = run_watched(
                sh("exec cat >/dev/null"),
                decoder_emitting(FRAMES),
                Arc::new(CancellableBatchControl::new()),
            )
            .expect("healthy pipeline");
            assert_eq!(r.exit_code, 0);
            assert!(!r.was_cancelled);
            assert_eq!(r.frame_count, FRAMES as u64);
        }

        /// The encoder exits before reading. The decoder (`cat`, which
        /// like ffmpeg only reads stdin while its stdout drains) still has
        /// most of the feed to pass through once we stop reading it.
        #[test]
        fn encoder_exit_does_not_hang_on_feeder() {
            let r = run_watched(
                sh("exit 1"),
                sh("exec cat"),
                Arc::new(CancellableBatchControl::new()),
            );
            assert!(
                !matches!(r, Ok(ref t) if t.exit_code == 0),
                "a dead encoder must not report success"
            );
        }

        /// Cancel with the decoder's output unread and the feed unfinished.
        #[test]
        fn cancel_does_not_hang_on_feeder() {
            let control = Arc::new(CancellableBatchControl::new());
            control
                .cancel_current
                .store(true, std::sync::atomic::Ordering::SeqCst);
            let r =
                run_watched(sh("exec cat >/dev/null"), sh("exec cat"), control).expect("cancel");
            assert!(r.was_cancelled);
        }

        /// Cancel while a frame write is blocked on an encoder that has
        /// stopped reading (a stalled encoder, or one stuck on a full
        /// stderr pipe).
        #[test]
        fn cancel_interrupts_blocked_encoder_write() {
            let control = Arc::new(CancellableBatchControl::new());
            let flip = Arc::clone(&control);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(500));
                flip.cancel_current
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            });
            let r = run_watched(sh("exec sleep 30"), decoder_emitting(FRAMES), control)
                .expect("cancel");
            assert!(r.was_cancelled);
        }

        /// An encoder that writes a lot to stderr before reading stdin
        /// (ffmpeg's stats and warnings over a long encode) must not stall
        /// the frame writes.
        #[test]
        fn encoder_stderr_is_drained() {
            let r = run_watched(
                sh("head -c 1048576 /dev/zero >&2; exec cat >/dev/null"),
                decoder_emitting(FRAMES),
                Arc::new(CancellableBatchControl::new()),
            )
            .expect("pipeline with a chatty encoder");
            assert_eq!(r.exit_code, 0);
            assert_eq!(r.frame_count, FRAMES as u64);
        }
    }

    // ── Property-based tests ─────────────────────────────────────

    use proptest::prelude::*;

    proptest! {
        #[test]
        fn canvas_new_no_panic(w in 0u32..=8192, h in 0u32..=8192) {
            // Should never panic - only Ok or Err
            let _ = Canvas::new(w, h);
        }

        #[test]
        fn canvas_composite_no_panic(
            canvas_w in 1u32..=64,
            canvas_h in 1u32..=64,
            frame_w in 1u32..=64,
            frame_h in 1u32..=64,
            x in 0u32..=128,
            y in 0u32..=128,
            blend in any::<bool>(),
        ) {
            if let Ok(mut canvas) = Canvas::new(canvas_w, canvas_h) {
                let frame_size = (frame_w as usize) * (frame_h as usize) * 4;
                let frame_data = vec![128u8; frame_size];
                // Should never panic even with out-of-bounds coordinates
                canvas.composite(&frame_data, x, y, frame_w, frame_h, blend);
            }
        }

        #[test]
        fn canvas_clear_rect_no_panic(
            canvas_w in 1u32..=64,
            canvas_h in 1u32..=64,
            x in 0u32..=128,
            y in 0u32..=128,
            w in 0u32..=128,
            h in 0u32..=128,
        ) {
            if let Ok(mut canvas) = Canvas::new(canvas_w, canvas_h) {
                // Should never panic even with out-of-bounds rectangle
                canvas.clear_rect(x, y, w, h);
            }
        }
    }
}
