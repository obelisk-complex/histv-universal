//! End-to-end output safety: run the real encode loop against a stub ffmpeg
//! that succeeds, fails or runs slowly on demand, and check what is left
//! on disk.
//!
//! The stub handles any command whose arguments contain `STUBSRC` (our test
//! sources) and passes everything else (capability checks) to the system
//! ffmpeg. For an encode it writes `ENCODED:<input path>` to the output, or,
//! when the input name contains `FAIL`, writes `partial` and exits 1. An
//! input name containing `SLOW` makes it wait 5 s first.
//!
//! Each file under tests/ is its own process, so `ffmpeg::init` and
//! `HISTV_TMP` are set once here without affecting other tests.

#![cfg(unix)]

use histv_lib::encoder::{run_encode_loop, BatchSettings, EncoderInfo};
use histv_lib::events::{BatchControl, EventSink};
use histv_lib::probe::ProbeResult;
use histv_lib::queue::{AudioStreamInfo, QueueItem, QueueItemStatus};
use histv_lib::staging::WaveItem;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, Once};

static SETUP: Once = Once::new();

/// Install the stub ffmpeg and a private staging dir, once per process.
fn setup() {
    SETUP.call_once(|| {
        let root = std::env::temp_dir().join(format!("histv-output-safety-{}", std::process::id()));
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let real = which("ffmpeg").unwrap_or_else(|| PathBuf::from("/bin/false"));
        let script = format!(
            r#"#!/bin/sh
case "$*" in *STUBSRC*) ;; *) exec "{real}" "$@" ;; esac
in=""; prev=""; last=""
for a in "$@"; do [ "$prev" = "-i" ] && in="$a"; prev="$a"; last="$a"; done
case "$in" in *SLOW*) sleep 5 ;; esac
case "$in" in *FAIL*) printf partial > "$last"; exit 1 ;; esac
printf 'ENCODED:%s' "$in" > "$last"
exit 0
"#,
            real = real.display()
        );
        let stub = bin.join("ffmpeg");
        std::fs::write(&stub, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_var("HISTV_TMP", root.join("staging"));
        histv_lib::ffmpeg::init(Some(&bin), None, &Sink::default());
    });
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(name))
            .find(|p| p.is_file())
    })
}

#[derive(Default)]
struct Sink {
    logs: Mutex<Vec<String>>,
    /// When a batch status line contains this text, Cancel All is pressed.
    cancel_on_status: Option<String>,
    cancel_all: AtomicBool,
}

impl EventSink for Sink {
    fn log(&self, m: &str) {
        self.logs.lock().unwrap().push(m.to_string());
    }
    fn file_progress(&self, _: f64, _: f64, _: f64, _: Option<(u8, u8)>) {}
    fn batch_progress(&self, _: u32, _: usize) {}
    fn batch_status(&self, s: &str) {
        if let Some(ref t) = self.cancel_on_status {
            if s.contains(t.as_str()) {
                self.cancel_all.store(true, Ordering::SeqCst);
            }
        }
    }
    fn queue_item_updated(&self, _: usize, _: &str) {}
    fn queue_item_probed(&self, _: usize, _: &QueueItem) {}
    fn batch_started(&self) {}
    fn batch_finished(&self, _: u32, _: u32, _: u32, _: &str) {}
    fn ffmpeg_stderr(&self, _: &str) {}
    fn batch_command(&self, _: &str) {}
    fn ffmpeg_download_progress(&self, _: &str) {}
    fn toast(&self, _: &str) {}
    fn post_batch(&self, _: &str, _: u32) {}
}

struct Control<'a> {
    sink: &'a Sink,
    overwrite: &'static str,
    fallback: &'static str,
    prompts: Mutex<Vec<String>>,
}

impl<'a> Control<'a> {
    fn new(sink: &'a Sink, overwrite: &'static str) -> Self {
        Self {
            sink,
            overwrite,
            fallback: "no",
            prompts: Mutex::new(Vec::new()),
        }
    }
}

impl BatchControl for Control<'_> {
    fn should_cancel_current(&self) -> bool {
        false
    }
    fn should_cancel_all(&self) -> bool {
        self.sink.cancel_all.load(Ordering::SeqCst)
    }
    fn is_paused(&self) -> bool {
        false
    }
    fn clear_cancel_current(&self) {}
    fn overwrite_always(&self) -> bool {
        false
    }
    fn set_overwrite_always(&self) {}
    fn overwrite_prompt(&self, path: &str) -> String {
        self.prompts.lock().unwrap().push(path.to_string());
        self.overwrite.to_string()
    }
    fn hw_fallback_offered(&self) -> bool {
        false
    }
    fn set_hw_fallback_offered(&self) {}
    fn fallback_prompt(&self, _: &str) -> String {
        self.fallback.to_string()
    }
}

fn settings(mode: &str, out: &Path, encoder: &str, family: &str) -> BatchSettings {
    BatchSettings {
        compatibility_mode: false,
        preserve_av1: false,
        precision_mode: false,
        output_folder: out.to_string_lossy().into_owned(),
        output_mode: mode.to_string(),
        threshold: 4.0,
        qp_i: 20,
        qp_p: 22,
        crf_val: 20,
        rate_control_mode: "QP".to_string(),
        pix_fmt: "yuv420p".to_string(),
        delete_source: false,
        save_log: false,
        post_command: None,
        peak_multiplier: 1.5,
        threads: 0,
        low_priority: false,
        force_local: false,
        video_encoder: encoder.to_string(),
        codec_family: family.to_string(),
        audio_encoder: "auto".to_string(),
        audio_cap: 640,
        output_container: "mkv".to_string(),
    }
}

fn encoders() -> Vec<EncoderInfo> {
    vec![
        EncoderInfo {
            name: "libx264".into(),
            codec_family: "h264".into(),
            is_hardware: false,
        },
        EncoderInfo {
            name: "h264_nvenc".into(),
            codec_family: "h264".into(),
            is_hardware: true,
        },
    ]
}

/// A source file (1000 bytes of filler) and its queue item, probed as a
/// 40 Mbps H.264 file so the decision is always a re-encode.
fn source(path: &Path) -> QueueItem {
    std::fs::write(path, vec![b's'; 1000]).unwrap();
    QueueItem {
        full_path: path.to_string_lossy().into_owned(),
        file_name: path.file_name().unwrap().to_string_lossy().into_owned(),
        base_name: path.file_stem().unwrap().to_string_lossy().into_owned(),
        status: QueueItemStatus::Pending,
        source_bytes: 1000,
        probe: ProbeResult {
            video_codec: "h264".into(),
            video_width: 320,
            video_height: 240,
            video_bitrate_bps: 40_000_000.0,
            video_bitrate_mbps: 40.0,
            is_hdr: false,
            color_transfer: "bt709".into(),
            audio_streams: vec![AudioStreamInfo {
                index: 0,
                codec: "aac".into(),
                bitrate_kbps: 128,
            }],
            duration_secs: 1.0,
            dovi_profile: None,
            dovi_bl_compat_id: None,
            has_hdr10plus: false,
            hdr10plus_profile: String::new(),
            video_rotation: 0,
            video_fps: 25.0,
            subtitle_stream_count: 0,
        },
    }
}

/// Names in `dir`, sorted (to spot stray staged files).
fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n != "ffmpeg_logs")
        .collect();
    v.sort();
    v
}

fn run(
    sink: &Sink,
    ctrl: &Control,
    queue: &mut [QueueItem],
    s: &BatchSettings,
    plan: Option<Vec<WaveItem>>,
) -> (u32, u32, u32, bool) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(run_encode_loop(
        sink,
        ctrl,
        queue,
        s,
        &encoders(),
        plan,
        None,
    ))
}

fn local_plan(n: usize) -> Option<Vec<WaveItem>> {
    Some(
        (0..n)
            .map(|queue_index| WaveItem::Local { queue_index })
            .collect(),
    )
}

#[test]
fn failed_encode_leaves_an_existing_output_byte_identical() {
    setup();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let mut queue = vec![source(&dir.path().join("STUBSRC-FAIL-a.mkv"))];
    let previous = out.join("STUBSRC-FAIL-a.mkv");
    std::fs::write(&previous, b"previous good output").unwrap();

    let sink = Sink::default();
    let ctrl = Control::new(&sink, "yes");
    let s = settings("folder", &out, "libx264", "h264");
    let (done, failed, _, _) = run(&sink, &ctrl, &mut queue, &s, local_plan(1));

    assert_eq!((done, failed), (0, 1));
    assert_eq!(queue[0].status, QueueItemStatus::Failed);
    assert_eq!(std::fs::read(&previous).unwrap(), b"previous good output");
    assert_eq!(names(&out), vec!["STUBSRC-FAIL-a.mkv"]);
}

#[test]
fn cancelled_encode_leaves_an_existing_output_byte_identical() {
    setup();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let mut queue = vec![source(&dir.path().join("STUBSRC-SLOW-c.mkv"))];
    let previous = out.join("STUBSRC-SLOW-c.mkv");
    std::fs::write(&previous, b"previous good output").unwrap();

    let sink = Sink {
        cancel_on_status: Some("[1/1]".into()),
        ..Sink::default()
    };
    let ctrl = Control::new(&sink, "yes");
    let s = settings("folder", &out, "libx264", "h264");
    let (done, _, _, cancelled) = run(&sink, &ctrl, &mut queue, &s, local_plan(1));

    assert_eq!(done, 0);
    assert!(cancelled);
    assert_eq!(std::fs::read(&previous).unwrap(), b"previous good output");
    assert_eq!(names(&out), vec!["STUBSRC-SLOW-c.mkv"]);
}

#[test]
fn successful_encode_replaces_the_existing_output_and_leaves_no_staged_file() {
    setup();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let src = dir.path().join("STUBSRC-ok.mkv");
    let mut queue = vec![source(&src)];
    let target = out.join("STUBSRC-ok.mkv");
    std::fs::write(&target, b"previous output").unwrap();

    let sink = Sink::default();
    let ctrl = Control::new(&sink, "yes");
    let s = settings("folder", &out, "libx264", "h264");
    let (done, failed, _, _) = run(&sink, &ctrl, &mut queue, &s, local_plan(1));

    assert_eq!((done, failed), (1, 0));
    assert_eq!(queue[0].status, QueueItemStatus::Done);
    let got = String::from_utf8(std::fs::read(&target).unwrap()).unwrap();
    assert!(got.starts_with("ENCODED:"), "{got}");
    assert_eq!(names(&out), vec!["STUBSRC-ok.mkv"]);
}

#[test]
fn replace_mode_asks_before_overwriting_a_different_file() {
    setup();
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("STUBSRC-sib.mp4");
    let mut queue = vec![source(&src)];
    let sibling = dir.path().join("STUBSRC-sib.mkv");
    std::fs::write(&sibling, b"a different film").unwrap();

    let sink = Sink::default();
    let ctrl = Control::new(&sink, "no");
    let s = settings("replace", dir.path(), "libx264", "h264");
    let (_, _, skipped, _) = run(&sink, &ctrl, &mut queue, &s, local_plan(1));

    assert_eq!(ctrl.prompts.lock().unwrap().len(), 1);
    assert_eq!(skipped, 1);
    assert_eq!(std::fs::read(&sibling).unwrap(), b"a different film");
    assert_eq!(std::fs::read(&src).unwrap(), vec![b's'; 1000]);
}

#[test]
fn folder_mode_refuses_an_output_that_is_the_source() {
    setup();
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("STUBSRC-self.mkv");
    let mut queue = vec![source(&src)];

    let sink = Sink::default();
    let ctrl = Control::new(&sink, "yes");
    // Output folder is the source's own folder, same name and container.
    let s = settings("folder", dir.path(), "libx264", "h264");
    let (done, failed, _, _) = run(&sink, &ctrl, &mut queue, &s, local_plan(1));

    assert_eq!((done, failed), (0, 1));
    assert_eq!(std::fs::read(&src).unwrap(), vec![b's'; 1000]);
}

#[test]
fn hardware_failure_that_stops_the_batch_is_counted_as_failed() {
    setup();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let mut queue = vec![
        source(&dir.path().join("STUBSRC-FAIL-hw.mkv")),
        source(&dir.path().join("STUBSRC-after-hw.mkv")),
    ];

    let sink = Sink::default();
    // Control::new declines the software fallback.
    let ctrl = Control::new(&sink, "yes");
    let s = settings("folder", &out, "h264_nvenc", "h264");
    let (done, failed, skipped, stopped) = run(&sink, &ctrl, &mut queue, &s, local_plan(2));

    assert_eq!((done, failed, skipped), (0, 1, 0));
    assert!(stopped);
    assert_eq!(queue[0].status, QueueItemStatus::Failed);
    assert_eq!(queue[1].status, QueueItemStatus::Pending);
    let logs = sink.logs.lock().unwrap();
    assert!(
        logs.iter()
            .any(|l| l.starts_with("Batch stopped (encoder failed). Done: 0, Failed: 1")),
        "{logs:?}"
    );
}
