//! Integration tests for ffmpeg::init() and ffmpeg::reinit().
//!
//! These run in a separate binary (each file under tests/ is its own
//! process) so the OnceLock statics start fresh.

use std::fs;

mod common;

#[cfg(target_os = "windows")]
const EXE_EXT: &str = ".exe";
#[cfg(not(target_os = "windows"))]
const EXE_EXT: &str = "";

struct NoopSink;

impl histv_lib::events::EventSink for NoopSink {
    fn log(&self, _: &str) {}
    fn file_progress(&self, _: f64, _: f64, _: f64, _: Option<(u8, u8)>) {}
    fn batch_progress(&self, _: u32, _: usize) {}
    fn batch_status(&self, _: &str) {}
    fn queue_item_updated(&self, _: usize, _: &str) {}
    fn queue_item_probed(&self, _: usize) {}
    fn batch_started(&self) {}
    fn batch_finished(&self, _: u32, _: u32, _: u32, _: &str) {}
    fn ffmpeg_stderr(&self, _: &str) {}
    fn batch_command(&self, _: &str) {}
    fn ffmpeg_download_progress(&self, _: &str) {}
    fn toast(&self, _: &str) {}
    fn post_batch(&self, _: &str, _: u32) {}
}

#[test]
fn test_init_then_reinit() {
    let sink = NoopSink;

    // Create temp dir with fake binaries for initial init
    let ffmpeg_name = format!("ffmpeg{EXE_EXT}");
    let ffprobe_name = format!("ffprobe{EXE_EXT}");

    let tmp1 = tempfile::tempdir().unwrap();
    let ffmpeg1 = tmp1.path().join(&ffmpeg_name);
    let ffprobe1 = tmp1.path().join(&ffprobe_name);
    fs::write(&ffmpeg1, b"fake-v1").unwrap();
    fs::write(&ffprobe1, b"fake-v1").unwrap();

    // init() resolves from the provided resource dir
    histv_lib::ffmpeg::init(Some(tmp1.path()), None, &sink);

    // After init, ffmpeg_command() should work (returns a Command)
    let cmd = histv_lib::ffmpeg::ffmpeg_command();
    let program = cmd.as_std().get_program().to_string_lossy();
    assert!(
        program.contains("ffmpeg"),
        "ffmpeg_command program should contain 'ffmpeg', got: {program}"
    );

    // Create a second temp dir for reinit
    let tmp2 = tempfile::tempdir().unwrap();
    let ffmpeg2 = tmp2.path().join(&ffmpeg_name);
    let ffprobe2 = tmp2.path().join(&ffprobe_name);
    fs::write(&ffmpeg2, b"fake-v2").unwrap();
    fs::write(&ffprobe2, b"fake-v2").unwrap();

    // reinit() should override the paths via the RwLock mechanism
    histv_lib::ffmpeg::reinit(Some(tmp2.path()), &sink);

    // After reinit, the command should point to the new location
    let cmd2 = histv_lib::ffmpeg::ffmpeg_command();
    // Use to_string_lossy (not Debug {:?}) to avoid escaped backslashes on Windows
    let program2 = cmd2.as_std().get_program().to_string_lossy();
    let expected = tmp2.path().to_str().unwrap();
    assert!(
        program2.contains(expected),
        "After reinit, ffmpeg should point to new dir. Got: {program2}"
    );
}

#[test]
fn test_init_without_resource_dir() {
    // This test runs in the same binary as above, but OnceLock is
    // already set from the other test. That's fine - init() is
    // idempotent (OnceLock::set returns Err on second call).
    let sink = NoopSink;
    histv_lib::ffmpeg::init(None, None, &sink);
    // Should not panic; ffmpeg_command returns something usable
    let _cmd = histv_lib::ffmpeg::ffmpeg_command();
}
