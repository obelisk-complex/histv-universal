//! CLI entry point for histv-cli.
//!
//! Parses arguments, resolves ffmpeg, detects encoders, collects files,
//! probes them, and either prints a dry-run plan or encodes them.
//!
//! `main()` is the thin orchestrator; heavy lifting is delegated to:
//!  - `collect_and_probe_files` — glob/walk + parallel ffprobe + MKV tag repair
//!  - `print_dry_run_plan`      — decision table, DV/HDR10+ warnings, disk est.
//!  - `run_batch`               — wave-based staging, encode loop, exit code

mod batch_control;
mod cli;
mod cli_sink;

use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Semaphore;

use histv_lib::encoder::{self, EncodeDecision, EncoderInfo};
use histv_lib::events::EventSink;
use histv_lib::queue::{self, QueueItem, QueueItemStatus};

fn main() {
    let mut args = cli::CliArgs::parse();
    let is_tty = std::io::IsTerminal::is_terminal(&std::io::stderr());

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

    // Create the CLI event sink (Arc-wrapped for sharing across probe tasks)
    let sink = Arc::new(cli_sink::CliSink::new(args.log_level.clone()));

    // Resolve ffmpeg/ffprobe binary paths (no resource dir for CLI)
    histv_lib::ffmpeg::init(None, None, &*sink);
    #[cfg(feature = "dovi")]
    histv_lib::dovi_tools::init(None, &*sink);

    // Check ffmpeg availability
    let rt = tokio::runtime::Runtime::new().expect("Could not create tokio runtime");

    if !rt.block_on(histv_lib::ffmpeg::is_available()) {
        eprintln!("ERROR: ffmpeg not found. Install ffmpeg and ensure it is on your PATH.");
        std::process::exit(4);
    }

    // Run encoder detection
    let (video_encoders, _audio_encoders) = rt.block_on(encoder::detect_encoders(&*sink));

    // Resolve which video encoder to use
    let video_encoder = resolve_encoder(&args, &video_encoders);
    let is_hw_encoder = video_encoders
        .iter()
        .find(|e| e.name == video_encoder)
        .map(|e| e.is_hardware)
        .unwrap_or(false);

    // ── File collection + probing ─────────────────────────────
    let mut queue_items = collect_and_probe_files(&args, &rt, &sink, is_tty);

    // ── Repair tags mode (early exit) ─────────────────────────
    if args.repair_tags || args.deep_repair {
        run_repair_tags(&args, &rt, &queue_items, &sink, is_tty);
        std::process::exit(0);
    }

    // ── Dry-run plan display ──────────────────────────────────
    let disk_info = {
        let output_path = match args.output_mode {
            cli::OutputMode::Beside | cli::OutputMode::Replace => queue_items
                .iter()
                .find(|item| item.status == QueueItemStatus::Pending)
                .and_then(|item| std::path::Path::new(&item.full_path).parent())
                .unwrap_or(std::path::Path::new("."))
                .to_path_buf(),
            cli::OutputMode::Folder => args.output.clone(),
        };
        histv_lib::disk_monitor::partition_free_space(&output_path)
    };

    print_dry_run_plan(
        &queue_items,
        &args,
        &video_encoders,
        &video_encoder,
        is_hw_encoder,
        disk_info,
        is_tty,
        &sink,
    );

    if args.dry_run {
        eprintln!();
        eprintln!("Dry run complete. No files were encoded.");
        std::process::exit(0);
    }

    // ── Batch execution ───────────────────────────────────────
    let exit_code = run_batch(
        &args,
        &rt,
        &mut queue_items,
        &video_encoders,
        &video_encoder,
        &sink,
    );

    std::process::exit(exit_code);
}

// ── Extracted orchestration functions ─────────────────────────

/// Collect input files (glob / directory walk), probe each one in parallel,
/// and apply lightweight MKV tag repair. Returns the full queue — callers
/// filter by `QueueItemStatus::Pending` as needed.
fn collect_and_probe_files(
    args: &cli::CliArgs,
    rt: &tokio::runtime::Runtime,
    sink: &Arc<cli_sink::CliSink>,
    is_tty: bool,
) -> Vec<QueueItem> {
    if args.inputs.is_empty() {
        eprintln!("No input files specified. Use histv-cli --help for usage.");
        std::process::exit(3);
    }

    let input_paths: Vec<String> = args
        .inputs
        .iter()
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

    // ── Batch probe (parallel, up to 8 concurrent) ────────────
    let total_files = queue_items.len();

    let semaphore = Arc::new(Semaphore::new(8));
    let mut handles = Vec::with_capacity(total_files);

    for (i, item) in queue_items.iter().enumerate() {
        let file_path = item.full_path.clone();
        let sem = semaphore.clone();
        let sink_ref = sink.clone();
        handles.push(rt.spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let result = histv_lib::probe::probe_file(&file_path, &*sink_ref).await;
            (i, file_path, result)
        }));
    }

    let mut probed_count: usize = 0;
    for handle in handles {
        let (i, file_path, result) = rt.block_on(handle).expect("probe task panicked");
        probed_count += 1;

        if !matches!(args.log_level, cli::LogLevel::Quiet) {
            if is_tty {
                eprint!("\rProbing {}/{}...", probed_count, total_files);
            } else {
                eprintln!("Probing {}/{}...", probed_count, total_files);
            }
        }

        match result {
            Ok(pr) => {
                // Lightweight MKV tag repair: fix stale statistics
                // so the queue shows the real bitrate from import.
                let repair = histv_lib::mkv_tags::repair_after_probe(
                    &file_path,
                    pr.duration_secs,
                    &pr.audio_streams,
                );
                queue_items[i].probe = pr;
                queue_items[i].status = QueueItemStatus::Pending;

                if let Some(bps) = repair {
                    let corrected_mbps = bps as f64 / 1_000_000.0;
                    queue_items[i].probe.video_bitrate_bps = bps as f64;
                    queue_items[i].probe.video_bitrate_mbps = corrected_mbps;
                }
            }
            Err(e) => {
                sink.log(&format!(
                    "  WARNING: Probe failed for {}: {e}",
                    queue_items[i].file_name
                ));
                queue_items[i].status = QueueItemStatus::Failed;
            }
        }
    }

    // Clear the probing line in TTY mode
    if !matches!(args.log_level, cli::LogLevel::Quiet) && is_tty {
        eprint!("\r\x1b[2K"); // Clear line
    }

    // Report probe failures
    let pending_count = queue_items
        .iter()
        .filter(|item| item.status == QueueItemStatus::Pending)
        .count();

    if pending_count == 0 {
        eprintln!("All files failed to probe. Nothing to encode.");
        std::process::exit(3);
    }

    let failed_count = total_files - pending_count;
    if failed_count > 0 {
        sink.log(&format!(
            "{} file{} failed to probe and will be skipped.",
            failed_count,
            if failed_count == 1 { "" } else { "s" },
        ));
    }

    queue_items
}

/// Run the `--repair-tags` / `--deep-repair` mode and print results.
fn run_repair_tags(
    args: &cli::CliArgs,
    rt: &tokio::runtime::Runtime,
    queue_items: &[QueueItem],
    sink: &cli_sink::CliSink,
    is_tty: bool,
) {
    let probed_items: Vec<&QueueItem> = queue_items
        .iter()
        .filter(|item| item.status == QueueItemStatus::Pending)
        .collect();

    let is_deep = args.deep_repair;
    let mode_label = if is_deep {
        "Deep repairing"
    } else {
        "Repairing"
    };
    eprintln!();
    eprintln!("{} MKV stream statistics tags...", mode_label);
    eprintln!();

    let mut repaired: u32 = 0;
    let mut skipped: u32 = 0;
    let total = probed_items.len();

    for (i, item) in probed_items.iter().enumerate() {
        let path = std::path::Path::new(&item.full_path);
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();

        if ext != "mkv" {
            eprintln!(
                "  ({}/{}) {} - skipped (not MKV)",
                i + 1,
                total,
                item.file_name
            );
            skipped += 1;
            continue;
        }

        if is_tty {
            eprint!("\r\x1b[2K  ({}/{}) {}...", i + 1, total, item.file_name);
        }

        let result = if is_deep {
            rt.block_on(histv_lib::mkv_tags::deep_repair(path, sink))
        } else {
            rt.block_on(histv_lib::mkv_tags::repair_file_tags(path, sink))
        };

        if is_tty {
            eprint!("\r\x1b[2K");
        }

        match result {
            Ok((n, bps)) if n > 0 => {
                let mbps = bps as f64 / 1_000_000.0;
                eprintln!(
                    "  ({}/{}) {} - updated {} tag{} (video: {:.2}Mbps)",
                    i + 1,
                    total,
                    item.file_name,
                    n,
                    if n == 1 { "" } else { "s" },
                    mbps
                );
                repaired += 1;
            }
            Ok(_) => {
                eprintln!(
                    "  ({}/{}) {} - no statistics tags to update",
                    i + 1,
                    total,
                    item.file_name
                );
                skipped += 1;
            }
            Err(e) => {
                eprintln!("  ({}/{}) {} - ERROR: {}", i + 1, total, item.file_name, e);
                skipped += 1;
            }
        }
    }

    eprintln!();
    eprintln!(
        "Repair complete. {} file{} updated, {} skipped.",
        repaired,
        if repaired == 1 { "" } else { "s" },
        skipped
    );
}

/// Print the full dry-run plan: decision table, DV/HDR10+ warnings,
/// disk-space estimate, and staging plan for remote mounts.
#[allow(clippy::too_many_arguments)]
fn print_dry_run_plan(
    queue_items: &[QueueItem],
    args: &cli::CliArgs,
    detected_encoders: &[EncoderInfo],
    video_encoder: &str,
    is_hw_encoder: bool,
    disk_info: Option<(u64, u64)>,
    is_tty: bool,
    _sink: &cli_sink::CliSink,
) {
    let probed_items: Vec<&QueueItem> = queue_items
        .iter()
        .filter(|item| item.status == QueueItemStatus::Pending)
        .collect();

    // ── Compute encoding decisions ────────────────────────────
    let rate_control_mode = args.rc.to_string().to_uppercase();
    let decisions: Vec<EncodeDecision> = probed_items
        .iter()
        .map(|item| {
            let source_ext = std::path::Path::new(&item.full_path)
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let resolved = encoder::resolve_file_settings(
                &item.probe.video_codec,
                &source_ext,
                &encoder::BatchSettings {
                    compatibility_mode: args.compat,
                    preserve_av1: args.preserve_av1,
                    precision_mode: args.precision_mode,
                    output_folder: String::new(),
                    output_mode: String::new(),
                    threshold: args.bitrate,
                    qp_i: args.qp_i,
                    qp_p: args.qp_p,
                    crf_val: args.crf,
                    rate_control_mode: rate_control_mode.clone(),
                    pix_fmt: String::new(),
                    delete_source: false,
                    save_log: false,
                    post_command: None,
                    peak_multiplier: args.peak_multiplier,
                    threads: 0,
                    low_priority: false,
                    force_local: false,
                    video_encoder: args.encoder.clone().unwrap_or_else(|| "auto".to_string()),
                    codec_family: args.codec.to_string(),
                    audio_encoder: args.audio.to_string(),
                    audio_cap: args.audio_cap,
                    output_container: args.container.to_string(),
                },
                detected_encoders,
            );
            encoder::decide_encode_strategy(
                item.probe.video_bitrate_mbps,
                args.bitrate,
                &item.probe.video_codec,
                &resolved.codec_family,
                &encoder::RateControlParams {
                    mode: &rate_control_mode,
                    qp_i: args.qp_i,
                    qp_p: args.qp_p,
                    crf_val: args.crf,
                },
                args.peak_multiplier,
            )
        })
        .collect();

    // ── Remote mount detection ────────────────────────────────
    let mut mount_cache = histv_lib::remote::MountCache::new();
    let remote_annotations: Vec<Option<String>> = probed_items
        .iter()
        .map(|item| match args.remote {
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
        })
        .collect();

    // ── Disk-space estimate ───────────────────────────────────
    let batch_estimate = histv_lib::disk_monitor::estimate_batch(&probed_items, &decisions);

    // ── Encoder label ─────────────────────────────────────────
    let encoder_label = if is_hw_encoder {
        format!("{} (HW)", video_encoder)
    } else {
        format!("{} (SW)", video_encoder)
    };

    let codec_display = match args.codec {
        cli::CodecFamily::Hevc => "HEVC",
        cli::CodecFamily::H264 => "H.264",
        cli::CodecFamily::Auto => "auto",
    };

    // Count decisions by type
    let copy_count = decisions
        .iter()
        .filter(|d| matches!(d, EncodeDecision::Copy))
        .count();
    let vbr_count = decisions
        .iter()
        .filter(|d| matches!(d, EncodeDecision::Vbr { .. }))
        .count();
    let quality_count = decisions
        .iter()
        .filter(|d| matches!(d, EncodeDecision::Cqp { .. } | EncodeDecision::Crf { .. }))
        .count();
    let remote_count = remote_annotations.iter().filter(|a| a.is_some()).count();

    // ANSI colour helpers — no-ops when not a TTY
    let dim = if is_tty { "\x1b[2m" } else { "" };
    let reset = if is_tty { "\x1b[0m" } else { "" };
    let bold = if is_tty { "\x1b[1m" } else { "" };
    let cyan = if is_tty { "\x1b[36m" } else { "" };
    let green = if is_tty { "\x1b[32m" } else { "" };
    let magenta = if is_tty { "\x1b[35m" } else { "" };

    eprintln!();
    eprintln!(
        "{}Encoding plan{} ({} files, target {}Mbps {} via {}):",
        bold,
        reset,
        probed_items.len(),
        args.bitrate,
        codec_display,
        encoder_label,
    );
    eprintln!();

    // Column headers
    eprintln!(
        "  {dim}{:<34} {:>10}  {:<10}  {:>11}  {:<7}  {:<16}  Mount{reset}",
        "File", "Bitrate", "Codec", "Resolution", "HDR", "Action",
    );
    eprintln!("  {dim}{}{reset}", "-".repeat(106),);

    // Print per-file plan
    for (i, (item, decision)) in probed_items.iter().zip(decisions.iter()).enumerate() {
        let resolution = format!("{}x{}", item.probe.video_width, item.probe.video_height);

        let hdr_label = hdr_type_label(item);

        let remote_tag = match &remote_annotations[i] {
            Some(annotation) => annotation.clone(),
            None => {
                if is_tty {
                    format!("{dim}local{reset}")
                } else {
                    "local".to_string()
                }
            }
        };

        // Colour the action based on decision type
        let action = short_decision(decision, args.bitrate);
        let coloured_action = match decision {
            EncodeDecision::Copy => format!("{green}{:<16}{reset}", action),
            EncodeDecision::Vbr { .. } => format!("{cyan}{:<16}{reset}", action),
            EncodeDecision::Cqp { .. } | EncodeDecision::Crf { .. } => {
                format!("{magenta}{:<16}{reset}", action)
            }
        };

        // Truncate codec to 10 chars for consistent spacing
        let codec_str = truncate_filename(&item.probe.video_codec, 10);

        eprintln!(
            "  {:<34} {:>8.2}Mbps  {:<10}  {:>11}  {:<7}  {}  {}",
            truncate_filename(&item.file_name, 34),
            item.probe.video_bitrate_mbps,
            codec_str,
            resolution,
            hdr_label,
            coloured_action,
            remote_tag,
        );
    }

    // Summary line
    eprintln!();
    let mut summary_parts: Vec<String> = Vec::new();
    if vbr_count > 0 {
        summary_parts.push(format!("{cyan}{} to encode (VBR){reset}", vbr_count));
    }
    if quality_count > 0 {
        summary_parts.push(format!(
            "{magenta}{} to transcode (quality){reset}",
            quality_count
        ));
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

    // DV/HDR10+ pre-flight warnings
    {
        let caps = histv_lib::dovi_tools::capabilities();
        let dv_count = probed_items
            .iter()
            .filter(|item| item.probe.dovi_profile.is_some())
            .count();
        let dv5_count = probed_items
            .iter()
            .filter(|item| item.probe.dovi_profile == Some(5))
            .count();
        let hdr10plus_count = probed_items
            .iter()
            .filter(|item| item.probe.has_hdr10plus)
            .count();

        let yellow = if is_tty { "\x1b[33m" } else { "" };

        if dv_count > 0 && !caps.can_package_dovi_mp4 {
            eprintln!(
                "  {yellow}{}WARNING:{reset} {} Dolby Vision file{} will fall back to HDR10 (MP4Box not found)",
                bold, dv_count, if dv_count == 1 { "" } else { "s" },
            );
        }
        if dv5_count > 0 {
            eprintln!(
                "  {yellow}{}WARNING:{reset} {} DV Profile 5 file{} will be encoded as HDR10 without mastering metadata",
                bold, dv5_count, if dv5_count == 1 { "" } else { "s" },
            );
        }
        if hdr10plus_count > 0 && !caps.can_process_hdr10plus {
            eprintln!(
                "  {yellow}{}WARNING:{reset} {} HDR10+ file{} will lose dynamic metadata (hdr10plus support not available)",
                bold, hdr10plus_count, if hdr10plus_count == 1 { "" } else { "s" },
            );
        }
    }

    // Disk-space estimate
    if let Some((total_bytes, free_bytes)) = disk_info {
        let used_pct = if total_bytes > 0 {
            ((total_bytes - free_bytes) as f64 / total_bytes as f64 * 100.0) as u32
        } else {
            0
        };

        let red = if is_tty { "\x1b[31m" } else { "" };

        eprintln!();
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
                format!(
                    "{} freed after batch",
                    histv_lib::disk_monitor::format_bytes(net.unsigned_abs())
                )
            } else {
                format!(
                    "{} additional after batch",
                    histv_lib::disk_monitor::format_bytes(net as u64)
                )
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
                format!(
                    "{} freed after batch",
                    histv_lib::disk_monitor::format_bytes(net_delete.unsigned_abs())
                )
            } else {
                format!(
                    "{} additional after batch",
                    histv_lib::disk_monitor::format_bytes(net_delete as u64)
                )
            };
            eprintln!(
                "  With --delete-source:  up to {} needed during encoding, {}",
                histv_lib::disk_monitor::format_bytes(peak_with_delete),
                net_desc,
            );
        }
    }

    // ── Dry-run staging plan ──────────────────────────────────
    if remote_count > 0 {
        // Build a dry-run wave plan to show grouping
        let dry_pending: Vec<usize> = probed_items.iter().enumerate().map(|(i, _)| i).collect();
        let staging_dir = histv_lib::staging::resolve_staging_dir(None);
        let remote_never = matches!(args.remote, cli::RemotePolicy::Never);

        let dry_plan = if matches!(args.remote, cli::RemotePolicy::Always) {
            // All files as one big set of waves
            use histv_lib::staging::WaveItem;
            let budget: u64 = histv_lib::disk_monitor::partition_free_space(&staging_dir)
                .map(|(_, free)| (free as f64 * 0.9) as u64)
                .unwrap_or(u64::MAX);
            let mut plan: Vec<WaveItem> = Vec::new();
            let mut wave_indices: Vec<usize> = Vec::new();
            let mut wave_bytes: u64 = 0;
            for &idx in &dry_pending {
                let file_bytes = queue_items[idx].source_bytes;
                if file_bytes >= budget {
                    if !wave_indices.is_empty() {
                        plan.push(WaveItem::Wave {
                            indices: std::mem::take(&mut wave_indices),
                            total_stage_bytes: wave_bytes,
                        });
                        wave_bytes = 0;
                    }
                    plan.push(WaveItem::Wave {
                        indices: vec![idx],
                        total_stage_bytes: file_bytes,
                    });
                    continue;
                }
                if wave_bytes + file_bytes > budget && !wave_indices.is_empty() {
                    plan.push(WaveItem::Wave {
                        indices: std::mem::take(&mut wave_indices),
                        total_stage_bytes: wave_bytes,
                    });
                    wave_bytes = 0;
                }
                wave_indices.push(idx);
                wave_bytes += file_bytes;
            }
            if !wave_indices.is_empty() {
                plan.push(WaveItem::Wave {
                    indices: wave_indices,
                    total_stage_bytes: wave_bytes,
                });
            }
            plan
        } else {
            histv_lib::staging::WavePlanner::plan(
                queue_items,
                &dry_pending,
                &mut mount_cache,
                &staging_dir,
                false,
                remote_never,
            )
        };

        let wave_count = dry_plan
            .iter()
            .filter(|item| matches!(item, histv_lib::staging::WaveItem::Wave { .. }))
            .count();
        let local_count = dry_plan
            .iter()
            .filter(|item| matches!(item, histv_lib::staging::WaveItem::Local { .. }))
            .count();

        if wave_count > 0 {
            eprintln!();
            eprintln!(
                "{}Staging plan:{} {} wave{} ({} remote file{}, {} local file{})",
                bold,
                reset,
                wave_count,
                if wave_count == 1 { "" } else { "s" },
                remote_count,
                if remote_count == 1 { "" } else { "s" },
                local_count,
                if local_count == 1 { "" } else { "s" },
            );
            let mut wave_num = 0u32;
            for item in &dry_plan {
                match item {
                    histv_lib::staging::WaveItem::Wave {
                        indices,
                        total_stage_bytes,
                    } => {
                        wave_num += 1;
                        eprintln!(
                            "  Wave {}: {} file{}, {} staged",
                            wave_num,
                            indices.len(),
                            if indices.len() == 1 { "" } else { "s" },
                            histv_lib::disk_monitor::format_bytes(*total_stage_bytes),
                        );
                    }
                    histv_lib::staging::WaveItem::Local { .. } => {}
                }
            }
            if local_count > 0 {
                eprintln!(
                    "  Local:  {} file{} (no staging)",
                    local_count,
                    if local_count == 1 { "" } else { "s" },
                );
            }
        }
    }
}

/// Execute the batch: set up output folder, disk monitor, wave plan,
/// run the encode loop, and return an appropriate exit code.
#[allow(clippy::too_many_arguments)]
fn run_batch(
    args: &cli::CliArgs,
    rt: &tokio::runtime::Runtime,
    queue_items: &mut [QueueItem],
    detected_encoders: &[EncoderInfo],
    video_encoder: &str,
    sink: &cli_sink::CliSink,
) -> i32 {
    // ── Pre-batch setup ───────────────────────────────────────

    // Create output folder if needed (only in "folder" mode)
    if matches!(args.output_mode, cli::OutputMode::Folder) {
        let out_path = std::path::Path::new(&args.output);
        if !out_path.exists() {
            if let Err(e) = std::fs::create_dir_all(out_path) {
                eprintln!(
                    "ERROR: Could not create output folder '{}': {e}",
                    args.output.display()
                );
                std::process::exit(4);
            }
        }
        // Verify writable
        let test_path = out_path.join(".histv_write_test");
        if let Err(e) = std::fs::write(&test_path, b"") {
            eprintln!(
                "ERROR: Output folder '{}' is not writable: {e}",
                args.output.display()
            );
            std::process::exit(4);
        }
        let _ = std::fs::remove_file(&test_path);
    }

    // Disk-aware mode
    let mut delete_source = args.delete_source;
    if args.disk_limit != "off" && !args.disk_limit.is_empty() && !delete_source {
        sink.log("--disk-limit implies --delete-source. Enabling for this batch.");
        delete_source = true;
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
    let batch_control =
        batch_control::CliBatchControl::new(args.overwrite.clone(), args.fallback.clone());

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
        video_encoder: video_encoder.to_string(),
        codec_family: args.codec.to_string(),
        audio_encoder: args.audio.to_string(),
        audio_cap: args.audio_cap,
        pix_fmt: if args.no_hdr {
            "yuv420p".to_string()
        } else {
            "p010le".to_string()
        },
        delete_source,
        save_log: args.save_log,
        post_command: args.post_command.clone(),
        peak_multiplier: args.peak_multiplier,
        threads: args.threads,
        low_priority: args.low_priority,
        precision_mode: args.precision_mode,
        compatibility_mode: args.compat,
        preserve_av1: args.preserve_av1,
        force_local: false,
    };

    // ── Wave-based remote staging + encoding loop ─────────────

    // Build wave plan: groups consecutive remote files into staging waves.
    let pending_indices: Vec<usize> = queue_items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.status == QueueItemStatus::Pending)
        .map(|(i, _)| i)
        .collect();

    let remote_never = matches!(args.remote, cli::RemotePolicy::Never);
    let force_local = batch_settings.force_local;

    let mut mount_cache = histv_lib::remote::MountCache::new();

    let wave_plan = if matches!(args.remote, cli::RemotePolicy::Always) {
        // All files treated as remote - put them all in waves
        use histv_lib::staging::WaveItem;
        let budget: u64 = histv_lib::disk_monitor::partition_free_space(&staging_dir)
            .map(|(_, free)| (free as f64 * 0.9) as u64)
            .unwrap_or(u64::MAX);

        let mut plan: Vec<WaveItem> = Vec::new();
        let mut wave_indices: Vec<usize> = Vec::new();
        let mut wave_bytes: u64 = 0;

        for &idx in &pending_indices {
            let file_bytes = queue_items[idx].source_bytes;
            if file_bytes >= budget {
                if !wave_indices.is_empty() {
                    plan.push(WaveItem::Wave {
                        indices: std::mem::take(&mut wave_indices),
                        total_stage_bytes: wave_bytes,
                    });
                    wave_bytes = 0;
                }
                plan.push(WaveItem::Wave {
                    indices: vec![idx],
                    total_stage_bytes: file_bytes,
                });
                continue;
            }
            if wave_bytes + file_bytes > budget && !wave_indices.is_empty() {
                plan.push(WaveItem::Wave {
                    indices: std::mem::take(&mut wave_indices),
                    total_stage_bytes: wave_bytes,
                });
                wave_bytes = 0;
            }
            wave_indices.push(idx);
            wave_bytes += file_bytes;
        }
        if !wave_indices.is_empty() {
            plan.push(WaveItem::Wave {
                indices: wave_indices,
                total_stage_bytes: wave_bytes,
            });
        }
        plan
    } else {
        histv_lib::staging::WavePlanner::plan(
            queue_items,
            &pending_indices,
            &mut mount_cache,
            &staging_dir,
            force_local,
            remote_never,
        )
    };

    eprintln!();

    // Run the encoding loop with the wave plan
    let (done, failed, _skipped, was_cancelled) = rt.block_on(encoder::run_encode_loop(
        sink,
        batch_control.as_ref(),
        queue_items,
        &batch_settings,
        detected_encoders,
        Some(wave_plan),
        disk_monitor.as_ref(),
    ));

    // ── Exit code ─────────────────────────────────────────────
    if was_cancelled {
        2
    } else if failed > 0 {
        1
    } else if done == 0 {
        3
    } else {
        0
    }
}

// ── Helper functions ───────────────────────────────────────────

/// Resolve which video encoder to use based on args and detected encoders.
fn resolve_encoder(args: &cli::CliArgs, video_encoders: &[EncoderInfo]) -> String {
    // If the user forced a specific encoder, use it
    if let Some(ref forced) = args.encoder {
        return forced.clone();
    }

    // Determine the target codec family
    let target_family = if args.compat {
        "h264"
    } else if args.preserve_av1 {
        "av1"
    } else {
        match args.codec {
            cli::CodecFamily::H264 => "h264",
            cli::CodecFamily::Hevc => "hevc",
            cli::CodecFamily::Auto => "hevc", // Auto defaults to HEVC for encoder lookup
        }
    };

    video_encoders
        .iter()
        .find(|e| e.codec_family == target_family)
        .map(|e| e.name.clone())
        .unwrap_or_else(|| encoder::software_fallback(target_family).to_string())
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
    if let Some(pm) = job.peak_multiplier {
        args.peak_multiplier = pm;
    }
    if let Some(ref rc) = job.rate_control {
        if let Ok(r) = rc.parse::<cli::RateControl>() {
            args.rc = r;
        }
    }
    if let Some(qi) = job.qp_i {
        args.qp_i = qi.min(51);
    }
    if let Some(qp) = job.qp_p {
        args.qp_p = qp.min(51);
    }
    if let Some(crf) = job.crf {
        args.crf = crf.min(51);
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
    if let Some(ref dr) = job.disk_resume {
        if let Ok(v) = dr.parse::<u8>() {
            args.disk_resume = Some(v);
        }
    }
    if let Some(compat) = job.compat {
        args.compat = compat;
    }
    if let Some(pav1) = job.preserve_av1 {
        args.preserve_av1 = pav1;
    }
    if job.post_command.is_some() {
        eprintln!("Warning: post_command in job files is ignored for security; use --post-command on the CLI instead");
    }
    if let Some(sl) = job.save_log {
        args.save_log = sl;
    }
    if let Some(t) = job.threads {
        args.threads = t.min(64);
    }
    if let Some(lp) = job.low_priority {
        args.low_priority = lp;
    }
    if let Some(pm) = job.precision_mode {
        args.precision_mode = pm;
    }
}

/// HDR type label for a queue item.
fn hdr_type_label(item: &QueueItem) -> &'static str {
    if let Some(p) = item.probe.dovi_profile {
        match p {
            5 => "DV5",
            7 => "DV7",
            8 => "DV8",
            _ => "DV",
        }
    } else if item.probe.has_hdr10plus {
        "HDR10+"
    } else if item.probe.is_hdr {
        if item.probe.color_transfer == "arib-std-b67" {
            "HLG"
        } else {
            "HDR10"
        }
    } else {
        "SDR"
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
