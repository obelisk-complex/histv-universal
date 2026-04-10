//! Async tests for StagingContext::stage_file().

struct NoopSink;
impl histv_lib::events::EventSink for NoopSink {
    fn log(&self, _: &str) {}
    fn file_progress(&self, _: f64, _: f64, _: f64, _: Option<(u8, u8)>) {}
    fn batch_progress(&self, _: u32, _: usize) {}
    fn batch_status(&self, _: &str) {}
    fn queue_item_updated(&self, _: usize, _: &str) {}
    fn queue_item_probed(&self, _: usize, _: &histv_lib::queue::QueueItem) {}
    fn batch_started(&self) {}
    fn batch_finished(&self, _: u32, _: u32, _: u32, _: &str) {}
    fn ffmpeg_stderr(&self, _: &str) {}
    fn batch_command(&self, _: &str) {}
    fn ffmpeg_download_progress(&self, _: &str) {}
    fn toast(&self, _: &str) {}
    fn post_batch(&self, _: &str, _: u32) {}
}

#[tokio::test]
async fn stage_file_copies_and_cleans_up() {
    let sink = NoopSink;
    let tmp = tempfile::tempdir().unwrap();

    // Create a small source file
    let source = tmp.path().join("input.mkv");
    std::fs::write(&source, b"fake video content for staging test").unwrap();

    let staging_dir = tmp.path().join("staging");

    let ctx = histv_lib::staging::StagingContext::stage_file(&source, &staging_dir, 0, &sink).await;

    assert!(ctx.is_some(), "stage_file should succeed");
    let mut ctx = ctx.unwrap();

    // Staged file should exist with correct naming
    let staged = ctx.local_path().to_path_buf();
    assert!(staged.exists(), "Staged file should exist");
    assert!(
        staged
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("0_"),
        "Staged file should be prefixed with queue index"
    );

    // Content should match
    let staged_content = std::fs::read(&staged).unwrap();
    assert_eq!(staged_content, b"fake video content for staging test");

    // Explicit cleanup
    ctx.cleanup(&sink);
    assert!(!staged.exists(), "Staged file should be cleaned up");
}

#[tokio::test]
async fn stage_file_nonexistent_source_returns_none() {
    let sink = NoopSink;
    let tmp = tempfile::tempdir().unwrap();
    let staging_dir = tmp.path().join("staging");

    let ctx = histv_lib::staging::StagingContext::stage_file(
        std::path::Path::new("/nonexistent/file.mkv"),
        &staging_dir,
        0,
        &sink,
    )
    .await;

    assert!(
        ctx.is_none(),
        "stage_file should return None for missing source"
    );

    // No leaked files in staging dir
    if staging_dir.exists() {
        let entries: Vec<_> = std::fs::read_dir(&staging_dir).unwrap().collect();
        assert!(entries.is_empty(), "No files should be left in staging dir");
    }
}

#[tokio::test]
async fn stage_file_creates_staging_dir() {
    let sink = NoopSink;
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("input.mkv");
    std::fs::write(&source, b"test").unwrap();

    // Staging dir doesn't exist yet
    let staging_dir = tmp.path().join("nested").join("staging");
    assert!(!staging_dir.exists());

    let ctx = histv_lib::staging::StagingContext::stage_file(&source, &staging_dir, 5, &sink).await;

    assert!(ctx.is_some());
    assert!(staging_dir.exists(), "Staging dir should be created");

    let ctx = ctx.unwrap();
    let name = ctx.local_path().file_name().unwrap().to_string_lossy();
    assert!(name.starts_with("5_"), "Queue index prefix should be 5");
}

#[tokio::test]
async fn stage_file_drop_cleans_up() {
    let sink = NoopSink;
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("input.mkv");
    std::fs::write(&source, b"data").unwrap();
    let staging_dir = tmp.path().join("staging");

    let staged_path;
    {
        let ctx = histv_lib::staging::StagingContext::stage_file(&source, &staging_dir, 0, &sink)
            .await
            .unwrap();
        staged_path = ctx.local_path().to_path_buf();
        assert!(staged_path.exists());
        // ctx dropped here
    }
    assert!(
        !staged_path.exists(),
        "Drop guard should clean up staged file"
    );
}
