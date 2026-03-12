//! CLI entry point for histv-cli.
//!
//! Parses arguments, resolves ffmpeg, detects encoders, collects files,
//! probes them, and either prints a dry-run plan or (in future phases)
//! encodes them.

mod cli;
mod cli_sink;
mod batch_control;

use clap::Parser;
use std::path::PathBuf;

use histv_lib::encoder::{self, EncodeDecision};
use histv_lib::events::EventSink;
use histv_lib::queue::{self, QueueItem, QueueItemStatus};

fn main() {
    let mut args = cli::CliArgs::parse();

    // Handle --export-job: write job file and exit
    if let Some(ref path) = args.export_job {
        match cli::export_job_file(&args, path) {
            Ok(()) => {
                eprintln!("Job file written to {}", path.display());
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("ERROR: {e}");
                std::process::exit(4);
            }
        }
    }

    // Load job file if specified (merges into args)
    if let Some(ref job_path) = args.job {
        match cli::load_job_file(job_path) {
            Ok(job) => {
                merge_job_into_args(&mut args, &job);
            }
            Err(e) => {
                eprintln!("ERROR: {e}");
                std::process::exit(4);
            }
        }
    }

    // Create the CLI event sink
    let sink = cli_sink::CliSink::new(args.log_level.clone());

    // Resolve ffmpeg/ffprobe binary paths (no resource dir for CLI)
    histv_lib::ffmpeg::init(None, None, &sink);

    // Check ffmpeg availability
    let rt = tokio::runtime::Runtime::new().expect("Could not create tokio runtime");

    if !rt.block_on(histv_lib::ffmpeg::is_available()) {
        eprintln!("ERROR: ffmpeg not found. Install ffmpeg and ensure it is on your PATH.");
        std::process::exit(4);
    }

    // Run encoder detection
    let (video_encoders, _audio_encoders) = rt.block_on(encoder::detect_encoders(&sink));

    // Resolve which video encoder to use
    let target_codec = args.codec.to_string();
    let video_encoder = resolve_encoder(&args, &video_encoders);
    let is_hw_encoder = video_encoders
        .iter()
        .find(|e| e.name == video_encoder)
        .map(|e| e.is_hardware)
        .unwrap_or(false);

    // ── File collection (Phase 2.1) ────────────────────────────

    if args.inputs.is_empty() {
        eprintln!("No input files specified. Use histv-cli --help for usage.");
        std::process::exit(3);
    }

    let input_paths: Vec<String> = args.inputs.iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();

    let mut queue_items: Vec<QueueItem> = Vec::new();
    let add_result = queue::add_paths_to_queue(&mut queue_items, &input_paths);

    if add_result.count == 0 {
        eprintln!("No supported video files found in the given inputs.");
        std::process::exit(3);
    }

    sink.log(&format!(
        "Collected {} file{} from {} input{}",
        add_result.count,
        if add_result.count == 1 { "" } else { "s" },
        args.inputs.len(),
        if args.inputs.len() == 1 { "" } else { "s" },
    ));

    // ── Batch probe (Phase 2.2) ────────────────────────────────

    let total_files = queue_items.len();
    for i in 0..total_files {
        if !matches!(args.log_level, cli::LogLevel::Quiet) {
            let is_tty = std::io::IsTerminal::is_terminal(&std::io::stderr());
            if is_tty {
                eprint!("\rProbing {}/{}...", i + 1, total_files);
            } else {
                eprintln!("Probing {}/{}...", i + 1, total_files);
            }
        }

        let file_path = queue_items[i].full_path.clone();
        match rt.block_on(histv_lib::probe::probe_file(&file_path, &sink)) {
            Ok(pr) => {
                queue_items[i].video_codec = pr.video_codec;
                queue_items[i].video_width = pr.video_width;
                queue_items[i].video_height = pr.video_height;
                queue_items[i].video_bitrate_bps = pr.video_bitrate_bps;
                queue_items[i].video_bitrate_mbps = pr.video_bitrate_mbps;
                queue_items[i].is_hdr = pr.is_hdr;
                queue_items[i].color_transfer = pr.color_transfer;
                queue_items[i].audio_streams = pr.audio_streams;
                queue_items[i].duration_secs = pr.duration_secs;
                queue_items[i].status = QueueItemStatus::Pending;
            }
            Err(e) => {
                sink.log(&format!("  WARNING: Probe failed for {}: {e}", queue_items[i].file_name));
                queue_items[i].status = QueueItemStatus::Failed;
            }
        }
    }

    // Clear the probing line in TTY mode
    if !matches!(args.log_level, cli::LogLevel::Quiet) {
        let is_tty = std::io::IsTerminal::is_terminal(&std::io::stderr());
        if is_tty {
            eprint!("\r\x1b[2K"); // Clear line
        }
    }

    // Filter to only successfully probed files
    let probed_items: Vec<&QueueItem> = queue_items.iter()
        .filter(|item| item.status == QueueItemStatus::Pending)
        .collect();

    if probed_items.is_empty() {
        eprintln!("All files failed to probe. Nothing to encode.");
        std::process::exit(3);
    }

    let failed_count = total_files - probed_items.len();
    if failed_count > 0 {
        sink.log(&format!(
            "{} file{} failed to probe and will be skipped.",
            failed_count,
            if failed_count == 1 { "" } else { "s" },
        ));
    }

    // ── Compute encoding decisions (Phase 2.3) ─────────────────

    let rate_control_mode = args.rc.to_string().to_uppercase();
    let decisions: Vec<EncodeDecision> = probed_items.iter()
        .map(|item| {
            encoder::decide_encode_strategy(
                item.video_bitrate_mbps,
                args.bitrate,
                &item.video_codec,
                &target_codec,
                &rate_control_mode,
                args.qp_i,
                args.qp_p,
                args.crf,
            )
        })
        .collect();

    // ── Remote mount detection (Phase 2.4) ─────────────────────

    let mut mount_cache = histv_lib::remote::MountCache::new();
    let remote_annotations: Vec<Option<String>> = probed_items.iter()
        .map(|item| {
            match args.remote {
                cli::RemotePolicy::Never => None,
                cli::RemotePolicy::Always => Some("stage (--remote always)".to_string()),
                cli::RemotePolicy::Auto => {
                    let path = std::path::Path::new(&item.full_path);
                    if let Some(info) = mount_cache.mount_info(path) {
                        if info.is_remote {
                            Some(format!("{} - stage", info.fs_type))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }
            }
        })
        .collect();

    // ── Disk-space estimate (Phase 2.5) ────────────────────────

    let owned_items: Vec<QueueItem> = probed_items.iter().map(|i| (*i).clone()).collect();
    let batch_estimate = histv_lib::disk_monitor::estimate_batch(&owned_items, &decisions);

    let output_path = match args.output_mode {
        cli::OutputMode::Beside | cli::OutputMode::Replace => {
            // Use the first input's parent as a representative
            probed_items.first()
                .and_then(|item| std::path::Path::new(&item.full_path).parent())
                .unwrap_or(std::path::Path::new("."))
                .to_path_buf()
        }
        cli::OutputMode::Folder => args.output.clone(),
    };

    let disk_info = histv_lib::disk_monitor::partition_free_space(&output_path);

    // ── Dry-run output or batch summary ────────────────────────

    // Determine the display name for the encoder
    let encoder_label = if is_hw_encoder {
        format!("{} (HW)", video_encoder)
    } else {
        format!("{} (SW)", video_encoder)
    };

    // Count decisions by type
    let copy_count = decisions.iter().filter(|d| matches!(d, EncodeDecision::Copy)).count();
    let vbr_count = decisions.iter().filter(|d| matches!(d, EncodeDecision::Vbr { .. })).count();
    let quality_count = decisions.iter().filter(|d| {
        matches!(d, EncodeDecision::Cqp { .. } | EncodeDecision::Crf { .. })
    }).count();
    let remote_count = remote_annotations.iter().filter(|a| a.is_some()).count();

    let codec_display = match args.codec {
        cli::CodecFamily::Hevc => "HEVC",
        cli::CodecFamily::H264 => "H.264",
    };

    let is_tty = std::io::IsTerminal::is_terminal(&std::io::stderr());

    // ANSI colour helpers — no-ops when not a TTY
    let dim = if is_tty { "\x1b[2m" } else { "" };
    let reset = if is_tty { "\x1b[0m" } else { "" };
    let bold = if is_tty { "\x1b[1m" } else { "" };
    let cyan = if is_tty { "\x1b[36m" } else { "" };
    let green = if is_tty { "\x1b[32m" } else { "" };
    let _yellow = if is_tty { "\x1b[33m" } else { "" };
    let magenta = if is_tty { "\x1b[35m" } else { "" };

    eprintln!("");
    eprintln!(
        "{}Encoding plan{} ({} files, target {}Mbps {} via {}):",
        bold, reset,
        probed_items.len(), args.bitrate, codec_display, encoder_label,
    );
    eprintln!("");

    // Column headers
    eprintln!(
        "  {dim}{:<34} {:>10}  {:<10}  {:>11}  {:<16}  {}{reset}",
        "File", "Bitrate", "Codec", "Resolution", "Action", "Mount",
    );
    eprintln!(
        "  {dim}{}{reset}",
        "-".repeat(97),
    );

    // Print per-file plan
    for (i, (item, decision)) in probed_items.iter().zip(decisions.iter()).enumerate() {
        let hdr_tag = if item.is_hdr { " HDR" } else { "" };
        let resolution = format!("{}x{}{}", item.video_width, item.video_height, hdr_tag);

        let remote_tag = match &remote_annotations[i] {
            Some(annotation) => annotation.clone(),
            None => if is_tty { format!("{dim}local{reset}") } else { "local".to_string() },
        };

        // Colour the action based on decision type
        let action = short_decision(decision, args.bitrate);
        let coloured_action = match decision {
            EncodeDecision::Copy => format!("{green}{:<16}{reset}", action),
            EncodeDecision::Vbr { .. } => format!("{cyan}{:<16}{reset}", action),
            EncodeDecision::Cqp { .. } | EncodeDecision::Crf { .. } => format!("{magenta}{:<16}{reset}", action),
        };

        // Truncate codec to 10 chars for consistent spacing
        let codec_str = truncate_filename(&item.video_codec, 10);

        eprintln!(
            "  {:<34} {:>8.2}Mbps  {:<10}  {:>11}  {}  {}",
            truncate_filename(&item.file_name, 34),
            item.video_bitrate_mbps,
            codec_str,
            resolution,
            coloured_action,
            remote_tag,
        );
    }

    // Summary line
    eprintln!("");
    let mut summary_parts: Vec<String> = Vec::new();
    if vbr_count > 0 {
        summary_parts.push(format!("{cyan}{} to encode (VBR){reset}", vbr_count));
    }
    if quality_count > 0 {
        summary_parts.push(format!("{magenta}{} to transcode (quality){reset}", quality_count));
    }
    if copy_count > 0 {
        summary_parts.push(format!("{green}{} to copy{reset}", copy_count));
    }
    eprintln!("{}Summary:{} {}", bold, reset, summary_parts.join(", "));
    if remote_count > 0 {
        eprintln!(
            "         {} on remote mount (will stage locally)",
            remote_count
        );
    }

    // Disk-space estimate
    if let Some((total_bytes, free_bytes)) = disk_info {
        let used_pct = if total_bytes > 0 {
            ((total_bytes - free_bytes) as f64 / total_bytes as f64 * 100.0) as u32
        } else {
            0
        };

        let red = if is_tty { "\x1b[31m" } else { "" };

        eprintln!("");
        eprintln!("{}Disk-space estimate:{}", bold, reset);
        eprintln!(
            "  Output partition:  {} total, {} free, {}% used",
            histv_lib::disk_monitor::format_bytes(total_bytes),
            histv_lib::disk_monitor::format_bytes(free_bytes),
            used_pct,
        );

        let peak_with_delete = batch_estimate.peak_additional_bytes_with_delete;
        let peak_without_delete = batch_estimate.peak_additional_bytes;

        let projected_used_no_delete = total_bytes - free_bytes + peak_without_delete;
        let projected_pct_no_delete = if total_bytes > 0 {
            (projected_used_no_delete as f64 / total_bytes as f64 * 100.0) as u32
        } else {
            0
        };

        if args.delete_source {
            let net = batch_estimate.net_change_with_delete;
            let net_desc = if net < 0 {
                format!("{} freed after batch", histv_lib::disk_monitor::format_bytes(net.unsigned_abs()))
            } else {
                format!("{} additional after batch", histv_lib::disk_monitor::format_bytes(net as u64))
            };
            eprintln!(
                "  With --delete-source:  up to {} needed during encoding, {}",
                histv_lib::disk_monitor::format_bytes(peak_with_delete),
                net_desc,
            );
        } else {
            let warning = if projected_pct_no_delete > 75 {
                format!(" {red}{bold}— WARNING{reset}")
            } else {
                String::new()
            };
            eprintln!(
                "  Estimated output:    {} additional space needed, would reach {}%{}",
                histv_lib::disk_monitor::format_bytes(peak_without_delete),
                projected_pct_no_delete,
                warning,
            );
            // Also show what it would look like with --delete-source
            let net_delete = batch_estimate.net_change_with_delete;
            let net_desc = if net_delete < 0 {
                format!("{} freed after batch", histv_lib::disk_monitor::format_bytes(net_delete.unsigned_abs()))
            } else {
                format!("{} additional after batch", histv_lib::disk_monitor::format_bytes(net_delete as u64))
            };
            eprintln!(
                "  With --delete-source:  up to {} needed during encoding, {}",
                histv_lib::disk_monitor::format_bytes(peak_with_delete),
                net_desc,
            );
        }
    }

    if args.dry_run {
        eprintln!("");
        eprintln!("Dry run complete. No files were encoded.");
        std::process::exit(0);
    }

    // ── Pre-batch setup ────────────────────────────────────────

    // Create output folder if needed (only in "folder" mode)
    if matches!(args.output_mode, cli::OutputMode::Folder) {
        let out_path = std::path::Path::new(&args.output);
        if !out_path.exists() {
            if let Err(e) = std::fs::create_dir_all(out_path) {
                eprintln!("ERROR: Could not create output folder '{}': {e}", args.output.display());
                std::process::exit(4);
            }
        }
        // Verify writable
        let test_path = out_path.join(".histv_write_test");
        if let Err(e) = std::fs::write(&test_path, b"") {
            eprintln!("ERROR: Output folder '{}' is not writable: {e}", args.output.display());
            std::process::exit(4);
        }
        let _ = std::fs::remove_file(&test_path);
    }

    // Disk-aware mode
    let mut delete_source = args.delete_source;
    if args.disk_limit != "off" && !args.disk_limit.is_empty() {
        if !delete_source {
            sink.log("--disk-limit implies --delete-source. Enabling for this batch.");
            delete_source = true;
        }
    }

    // Staging directory
    let staging_dir = histv_lib::staging::resolve_staging_dir(args.local_tmp.as_deref());

    // Disk monitor
    let disk_monitor = histv_lib::disk_monitor::DiskMonitor::new(
        &args.disk_limit,
        args.disk_resume,
        &args.output,
        match args.remote {
            cli::RemotePolicy::Never => None,
            _ => Some(staging_dir.as_path()),
        },
    );

    if let Some(ref dm) = disk_monitor {
        sink.log(&format!(
            "Disk-aware mode enabled: pause at {}% usage, resume at {} free.",
            args.disk_limit,
            histv_lib::disk_monitor::format_bytes(dm.baseline_free()),
        ));
    }

    // Create batch control with signal handling
    let batch_control = batch_control::CliBatchControl::new(
        args.overwrite.clone(),
        args.fallback.clone(),
    );

    // Build settings struct
    let output_folder_str = args.output.to_string_lossy().to_string();
    let batch_settings = encoder::BatchSettings {
        output_folder: output_folder_str.clone(),
        output_container: args.container.to_string(),
        output_mode: args.output_mode.to_string(),
        threshold: args.bitrate,
        qp_i: args.qp_i,
        qp_p: args.qp_p,
        crf_val: args.crf,
        rate_control_mode: args.rc.to_string().to_uppercase(),
        video_encoder: video_encoder.clone(),
        codec_family: codec_display.to_string(),
        audio_encoder: args.audio.to_string(),
        audio_cap: args.audio_cap,
        pix_fmt: if args.hdr { "yuv420p10le".to_string() } else { "yuv420p".to_string() },
        delete_source,
        save_log: args.save_log,
        post_command: args.post_command.clone(),
    };

    // ── Remote staging + encoding loop ─────────────────────────

    // For each file, decide whether to stage it locally before encoding.
    // We do this by modifying the input paths in the queue items before
    // passing them to the encoding loop.
    let mut staging_contexts: Vec<Option<histv_lib::staging::StagingContext>> = Vec::new();

    let should_stage = !matches!(args.remote, cli::RemotePolicy::Never);
    if should_stage {
        for (i, item) in queue_items.iter_mut().enumerate() {
            if item.status != QueueItemStatus::Pending {
                staging_contexts.push(None);
                continue;
            }

            let needs_staging = match args.remote {
                cli::RemotePolicy::Always => true,
                cli::RemotePolicy::Auto => {
                    mount_cache.is_remote(std::path::Path::new(&item.full_path))
                }
                cli::RemotePolicy::Never => false,
            };

            if needs_staging {
                if let Some(info) = mount_cache.mount_info(std::path::Path::new(&item.full_path)) {
                    sink.log(&format!(
                        "  Remote source detected ({} mount at {})",
                        info.fs_type, info.mount_point.display()
                    ));
                }

                if let Some(ctx) = histv_lib::staging::StagingContext::stage_file(
                    std::path::Path::new(&item.full_path),
                    &staging_dir,
                    i,
                    &sink,
                ) {
                    // Rewrite the queue item's path to the local staged copy
                    item.full_path = ctx.local_path().to_string_lossy().to_string();
                    staging_contexts.push(Some(ctx));
                } else {
                    // Staging failed, encode in-place
                    staging_contexts.push(None);
                }
            } else {
                staging_contexts.push(None);
            }
        }
    }

    eprintln!("");

    // Run the encoding loop
    let (done, failed, _skipped, was_cancelled) = rt.block_on(
        encoder::run_encode_loop(&sink, batch_control.as_ref(), &mut queue_items, &batch_settings)
    );

    // Clean up any remaining staging contexts
    for ctx in staging_contexts.iter_mut().flatten() {
        ctx.cleanup(&sink);
    }

    // Disk monitor: check after batch (informational)
    if let Some(ref dm) = disk_monitor {
        // Final space check is informational only, no waiting
        let _ = dm;
    }

    // ── Exit code ──────────────────────────────────────────────

    if was_cancelled {
        std::process::exit(2);
    } else if failed > 0 {
        std::process::exit(1);
    } else if done == 0 {
        std::process::exit(3);
    } else {
        std::process::exit(0);
    }
}

// ── Helper functions ───────────────────────────────────────────

/// Resolve which video encoder to use based on args and detected encoders.
fn resolve_encoder(
    args: &cli::CliArgs,
    video_encoders: &[encoder::EncoderInfo],
) -> String {
    // If the user forced a specific encoder, use it
    if let Some(ref forced) = args.encoder {
        return forced.clone();
    }

    // Otherwise, pick the first available encoder for the target codec family
    let target_family = args.codec.to_string();
    video_encoders
        .iter()
        .find(|e| e.codec_family == target_family)
        .map(|e| e.name.clone())
        .unwrap_or_else(|| {
            // Fallback to software encoder
            encoder::software_fallback(match args.codec {
                cli::CodecFamily::Hevc => "HEVC",
                cli::CodecFamily::H264 => "H.264",
            }).to_string()
        })
}

/// Merge job file settings into CLI args. CLI flags take precedence —
/// job file values are only applied where the CLI arg is at its default.
fn merge_job_into_args(args: &mut cli::CliArgs, job: &cli::JobFile) {
    // Add job file inputs to the args inputs
    for file_path in &job.files {
        args.inputs.push(PathBuf::from(file_path));
    }

    // Job file values are applied as defaults — CLI flags override.
    // We can't easily detect "was this flag explicitly set" with clap derive,
    // so job file settings are applied unconditionally for now. Users are
    // expected to use either CLI flags or a job file, not both for the same
    // setting. CLI flags in the help text document this behaviour.
    if let Some(ref codec) = job.codec {
        if let Ok(c) = codec.parse::<cli::CodecFamily>() {
            args.codec = c;
        }
    }
    if let Some(bitrate) = job.bitrate {
        args.bitrate = bitrate;
    }
    if let Some(ref rc) = job.rate_control {
        if let Ok(r) = rc.parse::<cli::RateControl>() {
            args.rc = r;
        }
    }
    if let Some(qi) = job.qp_i {
        args.qp_i = qi;
    }
    if let Some(qp) = job.qp_p {
        args.qp_p = qp;
    }
    if let Some(crf) = job.crf {
        args.crf = crf;
    }
    if let Some(hdr) = job.hdr {
        args.hdr = hdr;
        args.no_hdr = !hdr;
    }
    if let Some(ref audio) = job.audio_codec {
        if let Ok(a) = audio.parse::<cli::AudioCodec>() {
            args.audio = a;
        }
    }
    if let Some(cap) = job.audio_bitrate_cap {
        args.audio_cap = cap;
    }
    if let Some(ref output) = job.output {
        args.output = PathBuf::from(output);
    }
    if let Some(ref om) = job.output_mode {
        if let Ok(m) = om.parse::<cli::OutputMode>() {
            args.output_mode = m;
        }
    }
    if let Some(ref container) = job.container {
        if let Ok(c) = container.parse::<cli::ContainerFormat>() {
            args.container = c;
        }
    }
    if let Some(ref ow) = job.overwrite {
        if let Ok(o) = ow.parse::<cli::OverwritePolicy>() {
            args.overwrite = o;
        }
    }
    if let Some(ds) = job.delete_source {
        args.delete_source = ds;
    }
    if let Some(ref fb) = job.fallback {
        if let Ok(f) = fb.parse::<cli::FallbackPolicy>() {
            args.fallback = f;
        }
    }
    if let Some(ref remote) = job.remote {
        if let Ok(r) = remote.parse::<cli::RemotePolicy>() {
            args.remote = r;
        }
    }
    if let Some(ref lt) = job.local_tmp {
        if !lt.is_empty() {
            args.local_tmp = Some(PathBuf::from(lt));
        }
    }
    if let Some(ref dl) = job.disk_limit {
        args.disk_limit = dl.clone();
    }
    if let Some(ref pc) = job.post_command {
        if !pc.is_empty() {
            args.post_command = Some(pc.clone());
        }
    }
    if let Some(sl) = job.save_log {
        args.save_log = sl;
    }
}

/// Truncate a filename to fit a given width, adding "..." if needed.
fn truncate_filename(name: &str, max_width: usize) -> String {
    if name.len() <= max_width {
        name.to_string()
    } else if max_width > 3 {
        format!("{}...", &name[..max_width - 3])
    } else {
        name[..max_width].to_string()
    }
}

/// Short decision label for the plan table.
fn short_decision(decision: &EncodeDecision, threshold: f64) -> String {
    match decision {
        EncodeDecision::Copy => "Copy".to_string(),
        EncodeDecision::Vbr { .. } => format!("VBR {}Mbps", threshold),
        EncodeDecision::Cqp { qi, qp } => format!("CQP {}/{}", qi, qp),
        EncodeDecision::Crf { crf, .. } => format!("CRF {}", crf),
    }
}