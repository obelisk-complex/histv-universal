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
    source.with_file_name(format!("{name}.histv-bak-{}-{n}", std::process::id()))
}

/// `staged_output_path` with the process id and sequence number given.
pub fn staged_output_path_with(final_path: &Path, pid: u32, n: u64) -> PathBuf {
    let stem = final_path.file_stem().unwrap_or_default().to_string_lossy();
    let name = match final_path.extension() {
        Some(ext) => format!("{stem}.histv-tmp-{pid}-{n}.{}", ext.to_string_lossy()),
        None => format!("{stem}.histv-tmp-{pid}-{n}"),
    };
    final_path.with_file_name(name)
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
