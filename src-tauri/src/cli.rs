//! CLI argument parsing and job file handling.
//!
//! Defines the `#[derive(Parser)]` struct matching the flag surface from the
//! spec, plus JSON job file loading and export.

use clap::{Parser, ValueEnum};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Honey, I Shrunk The Vids — headless batch video encoder.
///
/// Encode video files from the command line using the same engine as the
/// HISTV desktop app. Supports hardware-accelerated encoding, smart bitrate
/// decisions, and batch processing via flags or JSON job files.
#[derive(Parser, Debug)]
#[command(name = "histv-cli", version, about, long_about = None)]
pub struct CliArgs {
    /// Files and/or folders to encode
    #[arg(trailing_var_arg = true)]
    pub inputs: Vec<PathBuf>,

    // ── Input ──────────────────────────────────────────────────

    /// Load settings and file list from a JSON job file
    #[arg(short = 'j', long = "job", value_name = "FILE")]
    pub job: Option<PathBuf>,

    /// Write current flags + inputs to a job file, then exit
    #[arg(long = "export-job", value_name = "FILE")]
    pub export_job: Option<PathBuf>,

    // ── Video ──────────────────────────────────────────────────

    /// Video codec family
    #[arg(short = 'c', long = "codec", default_value = "auto", value_name = "CODEC")]
    pub codec: CodecFamily,

    /// Force a specific encoder (e.g. hevc_nvenc, libx265).
    /// Omit to auto-detect best available.
    #[arg(short = 'e', long = "encoder", value_name = "NAME")]
    pub encoder: Option<String>,

    /// Target bitrate in Mbps
    #[arg(short = 'b', long = "bitrate", default_value = "4", value_name = "MBPS")]
    pub bitrate: f64,

    /// VBR peak ceiling as a multiplier of target bitrate (e.g. 1.5 = 150%)
    #[arg(long = "peak-multiplier", default_value = "1.5", value_name = "MULT")]
    pub peak_multiplier: f64,

    /// Rate-control for below-target transcodes
    #[arg(long = "rc", default_value = "qp", value_name = "MODE")]
    pub rc: RateControl,

    /// QP I-frame value
    #[arg(long = "qp-i", default_value = "20", value_name = "N")]
    pub qp_i: u32,

    /// QP P-frame value
    #[arg(long = "qp-p", default_value = "22", value_name = "N")]
    pub qp_p: u32,

    /// CRF value (software encoders only)
    #[arg(long = "crf", default_value = "20", value_name = "N")]
    pub crf: u32,

    /// Preserve 10-bit HDR (auto-detected per file if omitted)
    #[arg(long = "hdr", conflicts_with = "no_hdr")]
    pub hdr: bool,

    /// Force SDR output even for HDR sources
    #[arg(long = "no-hdr", conflicts_with = "hdr")]
    pub no_hdr: bool,

    /// Limit the number of CPU threads ffmpeg may use (0 = auto)
    #[arg(long = "threads", default_value = "0", value_name = "N")]
    pub threads: u32,

    /// Run ffmpeg at below-normal process priority so other tasks are
    /// not starved of CPU time
    #[arg(long = "low-priority")]
    pub low_priority: bool,

    /// Precision mode: probe CRF viability before encoding, use extended
    /// lookahead scaled to system RAM, and cap with maxrate. Falls back
    /// to CQP if CRF would produce a larger file than source. Software
    /// CRF only.
    #[arg(long = "precision")]
    pub precision_mode: bool,
	
	/// Convert all files to H.264/MP4 with AC3 audio for maximum device
    /// compatibility. Overrides --codec, --container, and --audio.
    #[arg(long = "compat", conflicts_with = "preserve_av1")]
    pub compat: bool,

    /// Keep AV1 sources as AV1 instead of converting to HEVC.
    #[arg(long = "preserve-av1", conflicts_with = "compat")]
    pub preserve_av1: bool,

    // ── Audio ──────────────────────────────────────────────────

    /// Audio codec
    #[arg(short = 'a', long = "audio", default_value = "auto", value_name = "CODEC")]
    pub audio: AudioCodec,

    /// Audio bitrate cap in kbps
    #[arg(long = "audio-cap", default_value = "640", value_name = "KBPS")]
    pub audio_cap: u32,

    // ── Output ─────────────────────────────────────────────────

    /// Output directory
    #[arg(short = 'o', long = "output", default_value = "./output", value_name = "DIR")]
    pub output: PathBuf,

    /// Output placement mode: folder (use --output dir), beside (create
    /// output/ subfolder next to each input), replace (encode in-place,
    /// replacing the source file)
    #[arg(long = "output-mode", default_value = "folder", value_name = "MODE")]
    pub output_mode: OutputMode,

    /// Output container format
    #[arg(long = "container", default_value = "auto", value_name = "FMT")]
    pub container: ContainerFormat,

    /// Overwrite policy when output file already exists
    #[arg(long = "overwrite", default_value = "ask", value_name = "POLICY")]
    pub overwrite: OverwritePolicy,

    /// Delete source files after successful encode
    #[arg(long = "delete-source")]
    pub delete_source: bool,

    // ── Behaviour ──────────────────────────────────────────────

    /// Probe all files and print the encoding plan, then exit
    #[arg(long = "dry-run")]
    pub dry_run: bool,

    /// Fix stale MKV stream statistics tags (BPS, NUMBER_OF_BYTES,
    /// DURATION) on already-encoded files. Probes each file, computes
    /// the correct values from the actual file size, and patches the
    /// tags in-place. No re-encoding. Exits after repair.
    #[arg(long = "repair-tags")]
    pub repair_tags: bool,

    /// Deep tag repair: scans every packet in the file to compute
    /// exact byte counts and frame counts, then patches all statistics
    /// tags. Speed depends on disk read speed. Useful for files with
    /// severely corrupted metadata.
    #[arg(long = "deep-repair")]
    pub deep_repair: bool,

    /// HW encoder failure policy
    #[arg(long = "fallback", default_value = "ask", value_name = "POLICY")]
    pub fallback: FallbackPolicy,

    /// Remote share handling
    #[arg(long = "remote", default_value = "auto", value_name = "POLICY")]
    pub remote: RemotePolicy,

    /// Local staging directory for remote files
    #[arg(long = "local-tmp", value_name = "DIR")]
    pub local_tmp: Option<PathBuf>,

    /// Pause encoding when output partition exceeds this usage percentage.
    /// Implies --delete-source. Use "off" to disable.
    #[arg(long = "disk-limit", default_value = "off", value_name = "PCT")]
    pub disk_limit: String,

    /// Resume encoding when partition usage drops below this percentage
    #[arg(long = "disk-resume", value_name = "PCT")]
    pub disk_resume: Option<u8>,

    /// Shell command to run after batch completes
    #[arg(long = "post-command", value_name = "CMD")]
    pub post_command: Option<String>,

    /// Save batch log to the output directory
    #[arg(long = "save-log")]
    pub save_log: bool,

    /// Verbosity level
    #[arg(long = "log-level", default_value = "normal", value_name = "LEVEL")]
    pub log_level: LogLevel,
}

// ── Value enums ────────────────────────────────────────────────

#[derive(Debug, Clone, ValueEnum, Serialize, Deserialize)]
pub enum CodecFamily {
    Auto,
    Hevc,
    H264,
}

impl std::fmt::Display for CodecFamily {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auto => write!(f, "auto"),
            Self::Hevc => write!(f, "hevc"),
            Self::H264 => write!(f, "h264"),
        }
    }
}

#[derive(Debug, Clone, ValueEnum, Serialize, Deserialize)]
pub enum RateControl {
    Qp,
    Crf,
}

impl std::fmt::Display for RateControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Qp => write!(f, "qp"),
            Self::Crf => write!(f, "crf"),
        }
    }
}

#[derive(Debug, Clone, ValueEnum, Serialize, Deserialize)]
pub enum AudioCodec {
	Auto,
    Ac3,
    Eac3,
    Aac,
    Copy,
}

impl std::fmt::Display for AudioCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
			Self::Auto => write!(f, "auto"),
            Self::Ac3 => write!(f, "ac3"),
            Self::Eac3 => write!(f, "eac3"),
            Self::Aac => write!(f, "aac"),
            Self::Copy => write!(f, "copy"),
        }
    }
}

#[derive(Debug, Clone, ValueEnum, Serialize, Deserialize)]
pub enum ContainerFormat {
	Auto,
    Mkv,
    Mp4,
}

impl std::fmt::Display for ContainerFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
			Self::Auto => write!(f, "auto"),
            Self::Mkv => write!(f, "mkv"),
            Self::Mp4 => write!(f, "mp4"),
        }
    }
}

#[derive(Debug, Clone, ValueEnum, Serialize, Deserialize)]
pub enum OutputMode {
    Folder,
    Beside,
    Replace,
}

impl std::fmt::Display for OutputMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Folder => write!(f, "folder"),
            Self::Beside => write!(f, "beside"),
            Self::Replace => write!(f, "replace"),
        }
    }
}

#[derive(Debug, Clone, ValueEnum, Serialize, Deserialize)]
pub enum OverwritePolicy {
    Ask,
    Yes,
    Skip,
}

impl std::fmt::Display for OverwritePolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ask => write!(f, "ask"),
            Self::Yes => write!(f, "yes"),
            Self::Skip => write!(f, "skip"),
        }
    }
}

#[derive(Debug, Clone, ValueEnum, Serialize, Deserialize)]
pub enum FallbackPolicy {
    Ask,
    Yes,
    No,
}

impl std::fmt::Display for FallbackPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ask => write!(f, "ask"),
            Self::Yes => write!(f, "yes"),
            Self::No => write!(f, "no"),
        }
    }
}

#[derive(Debug, Clone, ValueEnum, Serialize, Deserialize)]
pub enum RemotePolicy {
    Auto,
    Always,
    Never,
}

impl std::fmt::Display for RemotePolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auto => write!(f, "auto"),
            Self::Always => write!(f, "always"),
            Self::Never => write!(f, "never"),
        }
    }
}

#[derive(Debug, Clone, ValueEnum, Serialize, Deserialize)]
pub enum LogLevel {
    Quiet,
    Normal,
    Verbose,
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Quiet => write!(f, "quiet"),
            Self::Normal => write!(f, "normal"),
            Self::Verbose => write!(f, "verbose"),
        }
    }
}

// ── Job file ───────────────────────────────────────────────────

/// JSON job file schema. All fields are optional — omitted fields use CLI
/// defaults. CLI flags override job file values.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobFile {
    #[serde(default)]
    pub files: Vec<String>,
    pub codec: Option<String>,
    pub bitrate: Option<f64>,
    pub peak_multiplier: Option<f64>,
    pub rate_control: Option<String>,
    pub qp_i: Option<u32>,
    pub qp_p: Option<u32>,
    pub crf: Option<u32>,
    pub hdr: Option<bool>,
    pub audio_codec: Option<String>,
    pub audio_bitrate_cap: Option<u32>,
    pub output: Option<String>,
    pub output_mode: Option<String>,
    pub container: Option<String>,
    pub overwrite: Option<String>,
    pub delete_source: Option<bool>,
    pub fallback: Option<String>,
    pub remote: Option<String>,
    pub local_tmp: Option<String>,
    pub disk_limit: Option<String>,
    pub disk_resume: Option<String>,
    pub post_command: Option<String>,
    pub save_log: Option<bool>,
    pub threads: Option<u32>,
    pub low_priority: Option<bool>,
    pub precision_mode: Option<bool>,
	#[serde(default)]
    pub compat: Option<bool>,
    #[serde(default)]
    pub preserve_av1: Option<bool>,
}

/// Load a job file from disk.
pub fn load_job_file(path: &std::path::Path) -> Result<JobFile, String> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| format!("Could not read job file '{}': {e}", path.display()))?;
    serde_json::from_str(&contents)
        .map_err(|e| format!("Invalid job file '{}': {e}", path.display()))
}

/// Export current CLI args to a job file.
pub fn export_job_file(args: &CliArgs, path: &std::path::Path) -> Result<(), String> {
    let job = JobFile {
        files: args.inputs.iter().map(|p| p.to_string_lossy().to_string()).collect(),
        codec: Some(args.codec.to_string()),
        bitrate: Some(args.bitrate),
        peak_multiplier: Some(args.peak_multiplier),
        rate_control: Some(args.rc.to_string()),
        qp_i: Some(args.qp_i),
        qp_p: Some(args.qp_p),
        crf: Some(args.crf),
        hdr: if args.hdr { Some(true) } else if args.no_hdr { Some(false) } else { None },
        audio_codec: Some(args.audio.to_string()),
        audio_bitrate_cap: Some(args.audio_cap),
        output: Some(args.output.to_string_lossy().to_string()),
        output_mode: Some(args.output_mode.to_string()),
        container: Some(args.container.to_string()),
        overwrite: Some(args.overwrite.to_string()),
        delete_source: Some(args.delete_source),
        fallback: Some(args.fallback.to_string()),
        remote: Some(args.remote.to_string()),
        local_tmp: args.local_tmp.as_ref().map(|p| p.to_string_lossy().to_string()),
        disk_limit: Some(args.disk_limit.clone()),
        disk_resume: args.disk_resume.map(|v| v.to_string()),
        post_command: args.post_command.clone(),
        save_log: Some(args.save_log),
        threads: Some(args.threads),
        low_priority: Some(args.low_priority),
        precision_mode: Some(args.precision_mode),
    };

    let json = serde_json::to_string_pretty(&job)
        .map_err(|e| format!("Could not serialise job file: {e}"))?;
    std::fs::write(path, json)
        .map_err(|e| format!("Could not write job file '{}': {e}", path.display()))?;

    Ok(())
}

// ── FromStr implementations for job file parsing ───────────────

macro_rules! impl_fromstr_for_enum {
    ($enum_type:ty, $( $str_val:literal => $variant:expr ),+ $(,)?) => {
        impl std::str::FromStr for $enum_type {
            type Err = String;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s.to_lowercase().as_str() {
                    $( $str_val => Ok($variant), )+
                    _ => Err(format!("unknown value: '{}'", s)),
                }
            }
        }
    };
}

impl_fromstr_for_enum!(CodecFamily, "hevc" => CodecFamily::Hevc, "h264" => CodecFamily::H264);
impl_fromstr_for_enum!(RateControl, "qp" => RateControl::Qp, "crf" => RateControl::Crf);
impl_fromstr_for_enum!(AudioCodec, "ac3" => AudioCodec::Ac3, "eac3" => AudioCodec::Eac3, "aac" => AudioCodec::Aac, "copy" => AudioCodec::Copy);
impl_fromstr_for_enum!(ContainerFormat, "mkv" => ContainerFormat::Mkv, "mp4" => ContainerFormat::Mp4);
impl_fromstr_for_enum!(OutputMode, "folder" => OutputMode::Folder, "beside" => OutputMode::Beside, "replace" => OutputMode::Replace);
impl_fromstr_for_enum!(OverwritePolicy, "ask" => OverwritePolicy::Ask, "yes" => OverwritePolicy::Yes, "skip" => OverwritePolicy::Skip);
impl_fromstr_for_enum!(FallbackPolicy, "ask" => FallbackPolicy::Ask, "yes" => FallbackPolicy::Yes, "no" => FallbackPolicy::No);
impl_fromstr_for_enum!(RemotePolicy, "auto" => RemotePolicy::Auto, "always" => RemotePolicy::Always, "never" => RemotePolicy::Never);
impl_fromstr_for_enum!(LogLevel, "quiet" => LogLevel::Quiet, "normal" => LogLevel::Normal, "verbose" => LogLevel::Verbose);