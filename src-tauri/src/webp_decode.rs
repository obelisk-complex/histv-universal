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

use tokio::io::AsyncWriteExt;

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

    loop {
        let chunk_id = match read_fourcc(&mut file) {
            Ok(id) => id,
            Err(_) => break,
        };
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
                total_duration_ms += dur;
                frame_count += 1;
                // Skip the rest - no frame data read
            }
            _ => {}
        }

        let padded_size = chunk_size + (chunk_size & 1);
        let next = chunk_start + padded_size as u64;
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

    loop {
        let chunk_id = match read_fourcc(&mut file) {
            Ok(id) => id,
            Err(_) => break,
        };
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
                let data_size = chunk_size as usize - header_bytes;
                let mut data = vec![0u8; data_size];
                file.read_exact(&mut data)
                    .map_err(|e| format!("ANMF data: {e}"))?;

                total_duration_ms += dur;
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

        let padded_size = chunk_size + (chunk_size & 1);
        let next = chunk_start + padded_size as u64;
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
    fn new(width: u32, height: u32) -> Self {
        let size = (width * height * 4) as usize;
        Self {
            width,
            height,
            pixels: vec![0u8; size],
        }
    }

    /// Clear the entire canvas to transparent black.
    fn clear(&mut self) {
        self.pixels.fill(0);
    }

    /// Clear a rectangular region to transparent black.
    fn clear_rect(&mut self, x: u32, y: u32, w: u32, h: u32) {
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

    // Base framerate for the output. Individual frame durations are
    // honoured by writing each frame multiple times proportional to its
    // duration (frame duplication). 50fps = 20ms per tick, which is the
    // minimum frame duration in the WebP spec. The encoder compresses
    // duplicate frames to near-zero cost (identical P-frames).
    let base_fps: f64 = 50.0;
    let ms_per_tick: f64 = 1000.0 / base_fps;

    let canvas_w = info.width;
    let canvas_h = info.height;
    // Pad to even dimensions (required by most encoders)
    let padded_w = (canvas_w + 1) & !1;
    let padded_h = (canvas_h + 1) & !1;
    let frame_byte_size = (padded_w * padded_h * 4) as usize;

    // ── Spawn the output encoder ──
    let mut enc_args: Vec<String> = vec![
        "-y".into(),
        "-f".into(),
        "rawvideo".into(),
        "-pix_fmt".into(),
        "rgba".into(),
        "-s".into(),
        format!("{}x{}", padded_w, padded_h),
        "-r".into(),
        format!("{:.0}", base_fps),
        "-i".into(),
        "pipe:0".into(),
        "-an".into(),
        "-sn".into(),
    ];
    if threads > 0 {
        enc_args.push("-threads".into());
        enc_args.push(threads.to_string());
    }
    enc_args.extend(video_args.iter().cloned());
    enc_args.push("-pix_fmt".into());
    enc_args.push("yuv420p".into());
    if output_path.ends_with(".mp4") {
        enc_args.push("-movflags".into());
        enc_args.push("+faststart".into());
    }
    enc_args.push(output_path.to_string());

    let mut enc_cmd = crate::ffmpeg::ffmpeg_command();
    enc_cmd
        .args(&enc_args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());

    if low_priority {
        #[cfg(target_os = "windows")]
        {
            const BELOW_NORMAL: u32 = 0x00004000;
            const NO_WINDOW: u32 = 0x08000000;
            enc_cmd.creation_flags(BELOW_NORMAL | NO_WINDOW);
        }
    }

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

    // ── Compositing loop ──
    let mut canvas = Canvas::new(padded_w, padded_h);
    canvas.clear(); // WebP spec: canvas starts as transparent black
    let total_frames = frames.len();
    let mut decoded_count: u32 = 0;
    let mut was_cancelled = false;

    for (i, frame) in frames.iter().enumerate() {
        // Check for cancellation
        if batch_control.should_cancel_current() || batch_control.should_cancel_all() {
            was_cancelled = true;
            break;
        }

        // Decode this frame: wrap as standalone WebP, pipe to a decoder
        // ffmpeg instance, read back raw RGBA pixels.
        let standalone = wrap_as_standalone_webp(&frame.data);
        let decoded_rgba = decode_single_frame(standalone, frame.width, frame.height)
            .await
            .map_err(|e| format!("Frame {} decode failed: {e}", i + 1))?;

        // Dispose previous frame if needed (from the *previous* frame's flag)
        // WebP spec: disposal happens *before* rendering the current frame
        // if the *previous* frame had dispose=true.
        // (Handled below after composite for the current frame's flag)

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
        let mut pipe_broken = false;
        for _ in 0..repeat_count {
            if let Err(e) = enc_stdin.write_all(write_bytes).await {
                sink.log(&format!(
                    "  WARNING: Encoder pipe closed at frame {}: {e}",
                    i + 1
                ));
                pipe_broken = true;
                break;
            }
        }
        if pipe_broken {
            break;
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

    // Close encoder stdin to signal EOF
    drop(enc_stdin);

    // Wait for encoder to finish
    let enc_status = encoder
        .wait()
        .await
        .map_err(|e| format!("Encoder wait failed: {e}"))?;

    let exit_code = enc_status.code().unwrap_or(-1);

    Ok(TranscodeResult {
        exit_code,
        was_cancelled,
        frame_count: decoded_count as u64,
    })
}

/// Result from the animated WebP transcode pipeline.
pub struct TranscodeResult {
    pub exit_code: i32,
    pub was_cancelled: bool,
    pub frame_count: u64,
}

// ── Single-frame decoder ───────────────────────────────────────

/// Decode a single standalone WebP file (wrapped with RIFF header) to
/// raw RGBA pixels using ffmpeg's static WebP decoder.
///
/// Spawns a short-lived ffmpeg process that reads from stdin and writes
/// raw RGBA to stdout. This is fast for single frames (~10-50ms each).
async fn decode_single_frame(
    webp_data: Vec<u8>,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, String> {
    let expected_size = (width * height * 4) as usize;

    let mut cmd = crate::ffmpeg::ffmpeg_command();
    cmd.args([
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
    ])
    .stdin(std::process::Stdio::piped())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| format!("decode spawn: {e}"))?;

    // Write WebP data to stdin
    if let Some(mut stdin) = child.stdin.take() {
        let write_handle = tokio::spawn(async move {
            let _ = stdin.write_all(&webp_data).await;
            // stdin dropped here, closing the pipe
        });
        // Ensure write completes (and stdin closes) before reading output
        let _ = write_handle.await;
    }

    let output = child
        .wait_with_output()
        .await
        .map_err(|e| format!("decode wait: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("decode failed: {}", stderr.trim()));
    }

    if output.stdout.len() < expected_size {
        // Frame may be smaller than expected (partial frame, padding needed)
        let mut padded = vec![0u8; expected_size];
        let copy_len = output.stdout.len().min(expected_size);
        padded[..copy_len].copy_from_slice(&output.stdout[..copy_len]);
        Ok(padded)
    } else {
        Ok(output.stdout[..expected_size].to_vec())
    }
}
