//! Staged output install.
//!
//! ffmpeg never writes to an output's final path. Each encode goes to a
//! unique staged file in the same folder as its destination, and only a
//! finished encode is moved onto the final path, by a rename on the same
//! filesystem. A failed, cancelled or crashed encode therefore leaves any
//! earlier output exactly as it was, and never leaves a partial file under
//! the final name.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static STAGE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Longest staged or backup file name, in bytes. File systems allow 255;
/// the rest leaves room for the DV and HDR10+ working names derived from
/// a staged name (`.histv-dv.mp4`, `.hdr10p.<ext>`).
const MAX_NAME_BYTES: usize = 240;

/// `name` shortened, if needed, so that `name` + `reserved` bytes fit
/// MAX_NAME_BYTES, cut on a character boundary.
fn fit(name: &str, reserved: usize) -> &str {
    let budget = MAX_NAME_BYTES.saturating_sub(reserved);
    if name.len() <= budget {
        return name;
    }
    let mut end = budget;
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    &name[..end]
}

/// A staged path beside `final_path` that no other encode in any running
/// HISTV process uses. It keeps the final extension so ffmpeg picks the
/// same muxer.
pub fn staged_output_path(final_path: &Path) -> PathBuf {
    let n = STAGE_COUNTER.fetch_add(1, Ordering::Relaxed);
    staged_output_path_with(final_path, std::process::id(), n)
}

/// A name no other call in any running HISTV process returns, for a
/// per-file folder: `<prefix>-<pid>-<n>`.
pub fn unique_name(prefix: &str) -> String {
    let n = STAGE_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{}-{n}", std::process::id())
}

/// A backup name for `source` in its own folder, used while Replace mode
/// swaps the source for its encode. Unique, so a backup stranded by an
/// earlier crash is never overwritten.
pub fn backup_path(source: &Path) -> PathBuf {
    let n = STAGE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = source.file_name().unwrap_or_default().to_string_lossy();
    let suffix = format!(".histv-bak-{}-{n}", std::process::id());
    source.with_file_name(format!("{}{suffix}", fit(&name, suffix.len())))
}

/// `staged_output_path` with the process id and sequence number given.
pub fn staged_output_path_with(final_path: &Path, pid: u32, n: u64) -> PathBuf {
    let stem = final_path.file_stem().unwrap_or_default().to_string_lossy();
    let suffix = match final_path.extension() {
        Some(ext) => format!(".histv-tmp-{pid}-{n}.{}", ext.to_string_lossy()),
        None => format!(".histv-tmp-{pid}-{n}"),
    };
    // The staged name is never shown, so a long stem is simply shortened.
    final_path.with_file_name(format!("{}{suffix}", fit(&stem, suffix.len())))
}

/// What to do about a file already at the output path, decided before the
/// encode starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExistingOutput {
    /// Nothing to ask: no file there, or replace mode replacing its own source.
    Proceed,
    /// Another file is there: ask before overwriting it.
    Ask,
    /// The output path is the source file itself, outside replace mode.
    /// Encoding would replace the source, so the file must not be encoded.
    IsSource,
    /// The output path is the source of another file in the batch.
    IsOtherSource,
}

pub fn existing_output_action(
    output_exists: bool,
    output_is_source: bool,
    replace_mode: bool,
) -> ExistingOutput {
    match (output_exists, output_is_source, replace_mode) {
        (false, _, _) => ExistingOutput::Proceed,
        (true, true, true) => ExistingOutput::Proceed,
        (true, true, false) => ExistingOutput::IsSource,
        (true, false, _) => ExistingOutput::Ask,
    }
}

/// The same decision for delivering a staged remote encode to its real
/// destination, which also refuses to replace another file's source.
pub fn delivery_action(
    dest_exists: bool,
    dest_is_own_source: bool,
    dest_is_other_source: bool,
    replace_mode: bool,
) -> ExistingOutput {
    if dest_exists && dest_is_other_source {
        return ExistingOutput::IsOtherSource;
    }
    existing_output_action(dest_exists, dest_is_own_source, replace_mode)
}

/// True when both paths name the same existing file, including through a
/// symlink, a hard link or another spelling of the same folder. Falls back
/// to comparing the paths when either file cannot be read.
pub fn is_same_file(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(x), Ok(y)) = (std::fs::metadata(a), std::fs::metadata(b)) {
            return x.dev() == y.dev() && x.ino() == y.ino();
        }
    }
    #[cfg(not(unix))]
    {
        if let (Ok(x), Ok(y)) = (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
            return x == y;
        }
    }
    a == b
}

/// After Replace mode has put the encode at `output`, true when the source
/// is really gone: removed, or the output now stands at its path. False
/// when the source could not be moved aside and is still there.
pub fn source_was_replaced(source: &Path, output: &Path) -> bool {
    !source.exists() || is_same_file(source, output)
}

/// Identity of the file at a path: device and inode on Unix, size and
/// modification time elsewhere.
fn file_id(p: &Path) -> Option<(u64, u128)> {
    let m = std::fs::metadata(p).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some((m.dev(), m.ino() as u128))
    }
    #[cfg(not(unix))]
    {
        let t = m
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?;
        Some((m.len(), t.as_nanos()))
    }
}

/// A queued file as it was when the batch started. A path counts as this
/// source only while the original file is still there: once the batch has
/// installed an encode at that path (Replace mode, same name), the file
/// there is that item's output, not its source.
pub struct QueuedSource {
    path: PathBuf,
    id: Option<(u64, u128)>,
}

impl QueuedSource {
    pub fn new(path: PathBuf) -> Self {
        let id = file_id(&path);
        Self { path, id }
    }

    pub fn is_at(&self, p: &Path) -> bool {
        self.id.is_some() && is_same_file(p, &self.path) && file_id(p) == self.id
    }
}

/// Removes a staged encode when dropped, unless `keep` was called. Held
/// for the whole encode, so every early return (failure, cancel, launch
/// error) cleans up the staged file and only that file.
pub struct StagedFileGuard {
    path: PathBuf,
    armed: bool,
}

impl StagedFileGuard {
    pub fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    /// Hand the staged file on (to the post-encode install), so dropping
    /// the guard leaves it in place.
    pub fn keep(&mut self) {
        self.armed = false;
    }
}

impl Drop for StagedFileGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Move a finished staged encode onto its final path, replacing any older
/// output there. On error the staged file and any older output are left
/// as they were.
pub fn install(staged: &Path, final_path: &Path) -> std::io::Result<()> {
    if staged == final_path {
        return Ok(());
    }
    std::fs::rename(staged, final_path)
}

/// Copy a finished local encode to a staged file beside `dest`, ready for
/// `install`. Removes the partial copy if the copy fails.
pub async fn copy_beside(local: &Path, dest: &Path) -> std::io::Result<PathBuf> {
    let staged = staged_output_path(dest);
    match tokio::fs::copy(local, &staged).await {
        Ok(_) => Ok(staged),
        Err(e) => {
            let _ = tokio::fs::remove_file(&staged).await;
            Err(e)
        }
    }
}

/// Deliver a finished local encode to `dest` (for example a network share):
/// copy it beside `dest`, then install it. On error nothing is left at the
/// destination and the local file is kept.
pub async fn deliver(local: &Path, dest: &Path) -> std::io::Result<()> {
    let staged = copy_beside(local, dest).await?;
    if let Err(e) = install(&staged, dest) {
        let _ = std::fs::remove_file(&staged);
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staged_path_is_beside_final_and_keeps_extension() {
        let p = staged_output_path_with(Path::new("/out/My Film.v2.mkv"), 42, 7);
        assert_eq!(p, Path::new("/out/My Film.v2.histv-tmp-42-7.mkv"));
        let p = staged_output_path_with(Path::new("/out/noext"), 1, 0);
        assert_eq!(p, Path::new("/out/noext.histv-tmp-1-0"));
    }

    #[test]
    fn staged_and_backup_names_fit_the_file_name_limit() {
        // 2 + 3 * 82 = 248 bytes of stem, with 3-byte characters so a cut
        // at the byte budget would land inside one.
        let stem = format!("ab{}", "\u{20ac}".repeat(82));
        assert_eq!(stem.len(), 248);
        let fin = PathBuf::from(format!("/out/{stem}.mkv"));
        let a = staged_output_path(&fin);
        let b = staged_output_path(&fin);
        assert_ne!(a, b);
        for p in [&a, &b] {
            let name = p.file_name().unwrap().to_str().unwrap();
            // Room is left for the DV / HDR10+ names derived from it.
            assert!(name.len() <= MAX_NAME_BYTES, "{} bytes", name.len());
            assert!(name.ends_with(".mkv") && name.contains(".histv-tmp-"));
            // A derived name still fits the 255-byte limit.
            let dv = p.with_extension("histv-dv.mp4");
            assert!(dv.file_name().unwrap().len() <= 255);
        }
        let bak = backup_path(&fin);
        let name = bak.file_name().unwrap().to_str().unwrap();
        assert!(name.len() <= MAX_NAME_BYTES && name.contains(".histv-bak-"));
        // Short names are untouched.
        let short = staged_output_path_with(Path::new("/o/a.mkv"), 1, 2);
        assert_eq!(short, Path::new("/o/a.histv-tmp-1-2.mkv"));
    }

    #[test]
    fn staged_paths_never_repeat_for_the_same_destination() {
        let a = staged_output_path(Path::new("/out/movie.mkv"));
        let b = staged_output_path(Path::new("/out/movie.mkv"));
        assert_ne!(a, b);
        assert_ne!(a, Path::new("/out/movie.mkv"));
        assert_eq!(a.parent(), Some(Path::new("/out")));
    }

    #[test]
    fn backup_paths_are_unique_and_beside_the_source() {
        let a = backup_path(Path::new("/share/movie.avi"));
        let b = backup_path(Path::new("/share/movie.avi"));
        assert_ne!(a, b);
        assert_eq!(a.parent(), Some(Path::new("/share")));
        let name = a.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("movie.avi.histv-bak-"), "{name}");
    }

    #[test]
    fn existing_output_decision_table() {
        use ExistingOutput::*;
        assert_eq!(existing_output_action(false, false, false), Proceed);
        assert_eq!(existing_output_action(false, false, true), Proceed);
        assert_eq!(existing_output_action(true, false, false), Ask);
        // Replace mode: a different file at the output name (movie.mkv
        // beside source movie.avi) must be asked about, not overwritten.
        assert_eq!(existing_output_action(true, false, true), Ask);
        assert_eq!(existing_output_action(true, true, true), Proceed);
        assert_eq!(existing_output_action(true, true, false), IsSource);
    }

    #[test]
    fn delivery_decision_refuses_another_files_source() {
        use ExistingOutput::*;
        assert_eq!(delivery_action(false, false, true, true), Proceed);
        assert_eq!(delivery_action(true, false, true, true), IsOtherSource);
        assert_eq!(delivery_action(true, true, true, true), IsOtherSource);
        assert_eq!(delivery_action(true, true, false, true), Proceed);
        assert_eq!(delivery_action(true, false, false, true), Ask);
        assert_eq!(delivery_action(true, true, false, false), IsSource);
    }

    #[test]
    fn same_file_detects_links_and_distinguishes_copies() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.mkv");
        let b = dir.path().join("b.mkv");
        std::fs::write(&a, b"x").unwrap();
        std::fs::write(&b, b"x").unwrap();
        assert!(is_same_file(&a, &a));
        assert!(is_same_file(&a, &dir.path().join(".").join("a.mkv")));
        assert!(!is_same_file(&a, &b));
        let link = dir.path().join("link.mkv");
        std::fs::hard_link(&a, &link).unwrap();
        assert!(is_same_file(&a, &link));
        let missing = dir.path().join("missing.mkv");
        assert!(!is_same_file(&a, &missing));
    }

    #[test]
    fn source_counts_as_replaced_only_when_it_is_gone_or_is_the_output() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("movie.avi");
        let out = dir.path().join("movie.mkv");
        std::fs::write(&out, b"encode").unwrap();
        assert!(source_was_replaced(&src, &out)); // removed
        assert!(source_was_replaced(&out, &out)); // same name as output
        std::fs::write(&src, b"source").unwrap();
        assert!(!source_was_replaced(&src, &out)); // kept beside it
    }

    #[test]
    fn queued_source_stops_counting_once_an_encode_replaces_it() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("movie.mkv");
        std::fs::write(&src, b"source").unwrap();
        let queued = QueuedSource::new(src.clone());
        assert!(queued.is_at(&src));
        assert!(!queued.is_at(&dir.path().join("other.mkv")));

        let staged = dir.path().join("movie.histv-tmp-1-1.mkv");
        std::fs::write(&staged, b"encoded").unwrap();
        install(&staged, &src).unwrap();
        assert!(!queued.is_at(&src));
    }

    #[test]
    fn staged_file_guard_removes_only_its_own_file_unless_kept() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("movie.mkv");
        std::fs::write(&out, b"user output").unwrap();
        let staged = staged_output_path(&out);

        std::fs::write(&staged, b"partial").unwrap();
        drop(StagedFileGuard::new(staged.clone()));
        assert!(!staged.exists());
        assert_eq!(std::fs::read(&out).unwrap(), b"user output");

        std::fs::write(&staged, b"finished").unwrap();
        let mut g = StagedFileGuard::new(staged.clone());
        g.keep();
        drop(g);
        assert!(staged.exists());

        // Nothing written yet: dropping is harmless.
        drop(StagedFileGuard::new(dir.path().join("never-written.mkv")));
    }

    #[test]
    fn install_replaces_old_output_and_failed_install_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let fin = dir.path().join("out.mkv");
        std::fs::write(&fin, b"old").unwrap();
        let staged = staged_output_path(&fin);
        std::fs::write(&staged, b"new").unwrap();
        install(&staged, &fin).unwrap();
        assert_eq!(std::fs::read(&fin).unwrap(), b"new");
        assert!(!staged.exists());

        // A staged file that is not there: the output stays as it was.
        assert!(install(&staged, &fin).is_err());
        assert_eq!(std::fs::read(&fin).unwrap(), b"new");
    }

    #[tokio::test]
    async fn deliver_replaces_destination_and_failure_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("local.mkv");
        std::fs::write(&local, b"encoded").unwrap();
        let remote = dir.path().join("remote");
        std::fs::create_dir(&remote).unwrap();
        let dest = remote.join("movie.mkv");
        std::fs::write(&dest, b"older").unwrap();

        deliver(&local, &dest).await.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"encoded");
        assert_eq!(std::fs::read_dir(&remote).unwrap().count(), 1);
        assert!(local.exists());

        // Destination folder gone: the error is reported, the local file
        // is kept and nothing is written.
        let gone = dir.path().join("gone").join("movie.mkv");
        assert!(deliver(&local, &gone).await.is_err());
        assert!(local.exists());
        assert!(!dir.path().join("gone").exists());
    }
}
