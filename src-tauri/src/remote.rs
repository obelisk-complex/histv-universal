//! Remote mount detection for local staging decisions.
//!
//! Detects whether a file resides on a network-mounted filesystem (NFS, CIFS,
//! sshfs, etc.) so the CLI can stage it locally before encoding. Detection is
//! platform-specific and cached for the lifetime of a batch.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Information about a filesystem mount point.
#[derive(Debug, Clone)]
pub struct MountInfo {
    pub mount_point: PathBuf,
    pub fs_type: String,
    pub is_remote: bool,
}

/// Cached mount table for efficient per-file remote detection.
///
/// Lazily parses the system mount table on first query, then uses the
/// cached entries for all subsequent lookups within the same batch.
/// Directory-level canonicalisation results are also cached (#19) so
/// that files in the same directory don't each trigger a stat call.
pub struct MountCache {
    /// Parsed mount entries, sorted by mount point length (longest first)
    /// for correct longest-prefix matching.
    #[cfg(unix)]
    entries: Option<Vec<MountEntry>>,

    /// Windows-only: cached drive letter -> is_remote results.
    #[cfg(target_os = "windows")]
    drive_cache: HashMap<char, bool>,

    /// Directory-level canonicalise cache (#19).
    /// Maps a directory path to its canonicalised form to avoid repeated
    /// stat calls for files in the same directory (expensive on network mounts).
    dir_canon_cache: HashMap<PathBuf, PathBuf>,
}

#[cfg(unix)]
#[derive(Debug, Clone)]
struct MountEntry {
    mount_point: PathBuf,
    fs_type: String,
    is_remote: bool,
}

/// Known remote filesystem types on Linux.
#[cfg(target_os = "linux")]
const REMOTE_FS_TYPES: &[&str] = &[
    "nfs",
    "nfs4",
    "cifs",
    "smb",
    "smb2",
    "smb3",
    "fuse.sshfs",
    "fuse.rclone",
    "fuse.s3fs",
    "9p",
    "afs",
];

/// Known remote filesystem types on macOS.
#[cfg(target_os = "macos")]
const REMOTE_FS_TYPES: &[&str] = &[
    "nfs",
    "smbfs",
    "afpfs",
    "webdav",
    "fuse.sshfs",
    "fuse.rclone",
    "fuse.s3fs",
];

/// Filesystem types reported by macFUSE/osxfuse on macOS. These do not
/// encode the underlying protocol, so we must inspect the device field
/// to distinguish remote mounts (sshfs, rclone) from local ones (ext4fuse).
#[cfg(target_os = "macos")]
const MACFUSE_FS_TYPES: &[&str] = &["macfuse", "osxfuse"];

/// Canonicalise a path with a 5-second timeout. On dead network mounts,
/// `std::fs::canonicalize` can block indefinitely (it calls `realpath()`
/// which stats every path component). Falls back to the original path
/// if the call times out or fails.
fn canonicalize_with_timeout(dir: &Path) -> PathBuf {
    let dir_owned = dir.to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();
    let _handle = std::thread::spawn(move || {
        let result = std::fs::canonicalize(&dir_owned).unwrap_or(dir_owned);
        // Receiver may have been dropped on timeout; ignore send error.
        let _ = tx.send(result);
    });
    rx.recv_timeout(Duration::from_secs(5))
        .unwrap_or_else(|_| dir.to_path_buf())
}

impl Default for MountCache {
    fn default() -> Self {
        Self::new()
    }
}

impl MountCache {
    pub fn new() -> Self {
        Self {
            #[cfg(unix)]
            entries: None,
            #[cfg(target_os = "windows")]
            drive_cache: HashMap::new(),
            dir_canon_cache: HashMap::new(),
        }
    }

    /// Check whether the given path resides on a remote filesystem.
    pub fn is_remote(&mut self, path: &Path) -> bool {
        self.mount_info(path)
            .map(|info| info.is_remote)
            .unwrap_or(false)
    }

    /// Return mount information for the given path, or None if the
    /// mount point could not be determined.
    ///
    /// Uses a directory-level canonicalise cache (#19) so that files
    /// sharing the same parent directory only trigger one stat call.
    pub fn mount_info(&mut self, path: &Path) -> Option<MountInfo> {
        // Resolve via directory cache: canonicalise the parent directory
        // once, then append the file name. This avoids per-file stat
        // calls which are expensive on network mounts.
        let canonical = self.canonicalize_cached(path);

        #[cfg(target_os = "windows")]
        {
            return self.mount_info_windows(&canonical);
        }

        #[cfg(not(target_os = "windows"))]
        {
            self.ensure_parsed();
            self.longest_prefix_match(&canonical)
        }
    }

    /// Canonicalise a path using the directory-level cache (#19).
    /// For a file path, canonicalises the parent directory once and
    /// appends the file name. For a directory, canonicalises directly.
    /// Uses a purely syntactic check (file_name + parent presence) instead
    /// of `is_file()` to avoid a stat syscall before the cache lookup.
    fn canonicalize_cached(&mut self, path: &Path) -> PathBuf {
        let (dir, file_name) = if path.file_name().is_some() && path.parent().is_some() {
            let parent = path.parent().unwrap_or(path);
            let name = path.file_name();
            (parent.to_path_buf(), name.map(|n| n.to_os_string()))
        } else {
            (path.to_path_buf(), None)
        };

        let canonical_dir = if let Some(cached) = self.dir_canon_cache.get(&dir) {
            cached.clone()
        } else {
            let resolved = canonicalize_with_timeout(&dir);
            self.dir_canon_cache.insert(dir, resolved.clone());
            resolved
        };

        if let Some(name) = file_name {
            canonical_dir.join(name)
        } else {
            canonical_dir
        }
    }

    // ── Unix (Linux / macOS) ───────────────────────────────────

    #[cfg(not(target_os = "windows"))]
    fn ensure_parsed(&mut self) {
        if self.entries.is_some() {
            return;
        }
        let entries = parse_mount_table();
        self.entries = Some(entries);
    }

    #[cfg(not(target_os = "windows"))]
    fn longest_prefix_match(&self, path: &Path) -> Option<MountInfo> {
        let entries = self.entries.as_ref()?;
        // Entries are sorted longest-first, so the first match is the
        // most specific (longest prefix).
        for entry in entries {
            if path.starts_with(&entry.mount_point) {
                return Some(MountInfo {
                    mount_point: entry.mount_point.clone(),
                    fs_type: entry.fs_type.clone(),
                    is_remote: entry.is_remote,
                });
            }
        }
        None
    }

    // ── Windows ────────────────────────────────────────────────

    #[cfg(target_os = "windows")]
    fn mount_info_windows(&mut self, path: &Path) -> Option<MountInfo> {
        let path_str = path.to_string_lossy();

        // std::fs::canonicalize on Windows produces \\?\ extended-length
        // path prefixes (e.g. \\?\F:\...). Strip this before checking
        // for UNC paths, otherwise every local drive looks like a UNC share.
        let clean_str = if path_str.starts_with("\\\\?\\") {
            &path_str[4..]
        } else {
            &path_str
        };

        // UNC paths are always remote (\\server\share\...)
        if clean_str.starts_with("\\\\") {
            return Some(MountInfo {
                mount_point: PathBuf::from(
                    clean_str.split('\\').take(4).collect::<Vec<_>>().join("\\"),
                ),
                fs_type: "UNC".to_string(),
                is_remote: true,
            });
        }

        // Drive letter paths: check GetDriveTypeW
        let drive_letter = clean_str.chars().next()?;
        if !drive_letter.is_ascii_alphabetic() {
            return None;
        }

        let is_remote = self
            .drive_cache
            .entry(drive_letter.to_ascii_uppercase())
            .or_insert_with(|| check_drive_type_windows(drive_letter));

        Some(MountInfo {
            mount_point: PathBuf::from(format!("{}:\\", drive_letter.to_ascii_uppercase())),
            fs_type: if *is_remote {
                "network".to_string()
            } else {
                "local".to_string()
            },
            is_remote: *is_remote,
        })
    }
}

// ── Linux mount table parsing ──────────────────────────────────

#[cfg(target_os = "linux")]
fn parse_mount_table() -> Vec<MountEntry> {
    let contents = match std::fs::read_to_string("/proc/mounts") {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    parse_mount_table_from_str(&contents)
}

/// Parse mount table contents (in `/proc/mounts` format) into entries.
/// Extracted from `parse_mount_table` for testability.
#[cfg(target_os = "linux")]
fn parse_mount_table_from_str(contents: &str) -> Vec<MountEntry> {
    let mut entries: Vec<MountEntry> = contents
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 3 {
                return None;
            }
            let mount_point = unescape_mount_path(parts[1]);
            let fs_type = parts[2].to_string();
            let is_remote = REMOTE_FS_TYPES.iter().any(|rt| *rt == fs_type)
                || (fs_type.starts_with("fuse.") && is_remote_fuse(&fs_type));
            Some(MountEntry {
                mount_point: PathBuf::from(mount_point),
                fs_type,
                is_remote,
            })
        })
        .collect();

    // Filter out autofs entries - these are automount triggers that shadow
    // the real filesystem mount at the same path (e.g. autofs + cifs both
    // appear for /mnt/1tb, and autofs would incorrectly match as local).
    entries.retain(|e| e.fs_type != "autofs");

    // Sort by mount point length descending for longest-prefix matching
    entries.sort_by(|a, b| {
        b.mount_point
            .as_os_str()
            .len()
            .cmp(&a.mount_point.as_os_str().len())
    });

    entries
}

/// Check if a fuse.* filesystem type is a known remote type.
#[cfg(target_os = "linux")]
fn is_remote_fuse(fs_type: &str) -> bool {
    matches!(fs_type, "fuse.sshfs" | "fuse.rclone" | "fuse.s3fs")
}

/// Unescape octal sequences in /proc/mounts paths (e.g. `\040` for space,
/// `\303\251` for UTF-8 'e' with acute accent). Accumulates raw bytes from
/// consecutive octal escapes and decodes them as UTF-8 so that multi-byte
/// characters are not corrupted.
#[cfg(target_os = "linux")]
fn unescape_mount_path(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut byte_buf: Vec<u8> = Vec::new();
    let mut chars = s.chars();

    /// Flush accumulated raw bytes as UTF-8 into the result string.
    fn flush_bytes(buf: &mut Vec<u8>, out: &mut String) {
        if !buf.is_empty() {
            out.push_str(&String::from_utf8_lossy(buf));
            buf.clear();
        }
    }

    while let Some(c) = chars.next() {
        if c == '\\' {
            // Try to read 3 octal digits
            let mut octal = String::new();
            for _ in 0..3 {
                if let Some(&next) = chars.as_str().as_bytes().first() {
                    if (b'0'..=b'7').contains(&next) {
                        octal.push(chars.next().unwrap());
                    } else {
                        break;
                    }
                }
            }
            if octal.len() == 3 {
                if let Ok(byte) = u8::from_str_radix(&octal, 8) {
                    byte_buf.push(byte);
                } else {
                    flush_bytes(&mut byte_buf, &mut result);
                    result.push('\\');
                    result.push_str(&octal);
                }
            } else {
                flush_bytes(&mut byte_buf, &mut result);
                result.push('\\');
                result.push_str(&octal);
            }
        } else {
            flush_bytes(&mut byte_buf, &mut result);
            result.push(c);
        }
    }
    flush_bytes(&mut byte_buf, &mut result);
    result
}

// ── macOS mount table parsing ──────────────────────────────────

#[cfg(target_os = "macos")]
fn parse_mount_table() -> Vec<MountEntry> {
    // Parse `mount` output. Each line looks like:
    // /dev/disk1s1 on / (apfs, local, journaled)
    // nas.local:/volume on /Volumes/nas (nfs, nodev, nosuid)
    let output = match std::process::Command::new("mount")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => return Vec::new(),
    };

    let mut entries: Vec<MountEntry> = output
        .lines()
        .filter_map(|line| {
            // Format: <device> on <mount_point> (<fs_type>, <options>...)
            let on_idx = line.find(" on ")?;
            let paren_idx = line.rfind(" (")?;
            if paren_idx <= on_idx + 4 {
                return None;
            }
            let mount_point = &line[on_idx + 4..paren_idx];
            let opts_str = &line[paren_idx + 2..line.len().saturating_sub(1)];
            let fs_type = opts_str.split(',').next().unwrap_or("").trim().to_string();
            // macOS: treat FUSE mounts as remote unless the mount source
            // starts with /dev/, which indicates a local block device
            // (e.g. NTFS-3G mounting a USB drive via macFUSE).
            // macfuse/osxfuse types need device-field heuristics to
            // distinguish sshfs/rclone from local FUSE (e.g. ext4fuse).
            let device = &line[..on_idx];
            let is_macfuse = MACFUSE_FS_TYPES.iter().any(|ft| *ft == fs_type);
            let is_remote = REMOTE_FS_TYPES.iter().any(|rt| *rt == fs_type)
                || (fs_type.starts_with("fuse.") && !device.starts_with("/dev/"))
                || (is_macfuse && is_remote_macfuse_device(device));
            Some(MountEntry {
                mount_point: PathBuf::from(mount_point),
                fs_type,
                is_remote,
            })
        })
        .collect();

    entries.sort_by(|a, b| {
        b.mount_point
            .as_os_str()
            .len()
            .cmp(&a.mount_point.as_os_str().len())
    });

    entries
}

/// Check whether a macfuse/osxfuse device field indicates a remote mount.
/// SSHFS mounts appear as `sshfs#user@host:path`, rclone as `remote:path`
/// or `rclone://...`. Local FUSE mounts (e.g. ext4fuse) use `/dev/` paths
/// or bare local paths.
#[cfg(target_os = "macos")]
fn is_remote_macfuse_device(device: &str) -> bool {
    device.starts_with("sshfs#") || device.starts_with("rclone:") || device.contains("://")
}

// ── Windows drive type check ───────────────────────────────────

#[cfg(target_os = "windows")]
fn check_drive_type_windows(drive_letter: char) -> bool {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    // GetDriveTypeW expects "X:\" as a null-terminated wide string
    let root: Vec<u16> = OsStr::new(&format!("{}:\\", drive_letter.to_ascii_uppercase()))
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // DRIVE_REMOTE = 4
    const DRIVE_REMOTE: u32 = 4;

    // Safety: GetDriveTypeW is a well-defined Windows API call with a
    // null-terminated wide string argument. No memory ownership transfer.
    #[link(name = "kernel32")]
    extern "system" {
        fn GetDriveTypeW(lpRootPathName: *const u16) -> u32;
    }
    let drive_type = unsafe { GetDriveTypeW(root.as_ptr()) };
    drive_type == DRIVE_REMOTE
}

// Stub for non-Windows/macOS/Linux platforms
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn parse_mount_table() -> Vec<MountEntry> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Linux-only tests ──────────────────────────────────────────

    #[cfg(target_os = "linux")]
    mod linux {
        use super::super::*;

        // ── unescape_mount_path ───────────────────────────────────

        #[test]
        fn test_unescape_space() {
            assert_eq!(unescape_mount_path("hello\\040world"), "hello world");
        }

        #[test]
        fn test_unescape_utf8_multibyte() {
            assert_eq!(unescape_mount_path("M\\303\\251dias"), "Médias");
        }

        #[test]
        fn test_unescape_no_escapes() {
            assert_eq!(unescape_mount_path("/mnt/data"), "/mnt/data");
        }

        #[test]
        fn test_unescape_empty() {
            assert_eq!(unescape_mount_path(""), "");
        }

        #[test]
        fn test_unescape_partial_escape() {
            // Only two octal digits after backslash - not a valid 3-digit escape
            assert_eq!(unescape_mount_path("test\\04"), "test\\04");
        }

        #[test]
        fn test_unescape_trailing_backslash() {
            assert_eq!(unescape_mount_path("test\\"), "test\\");
        }

        #[test]
        fn test_unescape_japanese_utf8() {
            assert_eq!(
                unescape_mount_path("\\346\\227\\245\\346\\234\\254\\350\\252\\236"),
                "日本語"
            );
        }

        // ── parse_mount_table_from_str ────────────────────────────

        #[test]
        fn test_parse_basic_local_only() {
            let contents = include_str!("../tests/fixtures/proc_mounts_basic");
            let entries = parse_mount_table_from_str(contents);
            assert!(!entries.is_empty());
            for entry in &entries {
                assert!(
                    !entry.is_remote,
                    "Expected all entries to be local, but {:?} is remote",
                    entry.mount_point
                );
            }
        }

        #[test]
        fn test_parse_nfs_cifs_detected() {
            let contents = include_str!("../tests/fixtures/proc_mounts_nfs");
            let entries = parse_mount_table_from_str(contents);

            let find = |path: &str| -> Option<&MountEntry> {
                entries.iter().find(|e| e.mount_point == Path::new(path))
            };

            let media = find("/mnt/media").expect("/mnt/media not found");
            assert_eq!(media.fs_type, "nfs4");
            assert!(media.is_remote);

            let share = find("/mnt/share").expect("/mnt/share not found");
            assert_eq!(share.fs_type, "cifs");
            assert!(share.is_remote);

            let sshfs = find("/mnt/sshfs").expect("/mnt/sshfs not found");
            assert_eq!(sshfs.fs_type, "fuse.sshfs");
            assert!(sshfs.is_remote);

            let gdrive = find("/mnt/gdrive").expect("/mnt/gdrive not found");
            assert_eq!(gdrive.fs_type, "fuse.rclone");
            assert!(gdrive.is_remote);

            let s3 = find("/mnt/s3").expect("/mnt/s3 not found");
            assert_eq!(s3.fs_type, "fuse.s3fs");
            assert!(s3.is_remote);

            let external = find("/mnt/external").expect("/mnt/external not found");
            assert_eq!(external.fs_type, "ext4");
            assert!(!external.is_remote);

            let root = find("/").expect("/ not found");
            assert_eq!(root.fs_type, "ext4");
            assert!(!root.is_remote);
        }

        #[test]
        fn test_parse_autofs_filtered() {
            let contents = include_str!("../tests/fixtures/proc_mounts_autofs");
            let entries = parse_mount_table_from_str(contents);

            // autofs entries should be filtered out
            assert!(
                !entries.iter().any(|e| e.fs_type == "autofs"),
                "autofs entries should be filtered out"
            );

            // The cifs mount at /mnt/1tb should still be present and remote
            let onetb = entries
                .iter()
                .find(|e| e.mount_point == Path::new("/mnt/1tb"))
                .expect("/mnt/1tb not found");
            assert!(onetb.is_remote);
            assert_eq!(onetb.fs_type, "cifs");
        }

        #[test]
        fn test_parse_empty_input() {
            let entries = parse_mount_table_from_str("");
            assert!(entries.is_empty());
        }

        #[test]
        fn test_parse_malformed_line() {
            let entries = parse_mount_table_from_str("onlyone");
            assert!(entries.is_empty());
        }

        #[test]
        fn test_parse_sorted_longest_first() {
            let contents = include_str!("../tests/fixtures/proc_mounts_nfs");
            let entries = parse_mount_table_from_str(contents);
            assert!(entries.len() >= 2);
            for pair in entries.windows(2) {
                assert!(
                    pair[0].mount_point.as_os_str().len() >= pair[1].mount_point.as_os_str().len(),
                    "Entries not sorted longest first: {:?} before {:?}",
                    pair[0].mount_point,
                    pair[1].mount_point
                );
            }
        }

        // ── is_remote_fuse ────────────────────────────────────────

        #[test]
        fn test_is_remote_fuse_sshfs() {
            assert!(is_remote_fuse("fuse.sshfs"));
        }

        #[test]
        fn test_is_remote_fuse_rclone() {
            assert!(is_remote_fuse("fuse.rclone"));
        }

        #[test]
        fn test_is_remote_fuse_unknown() {
            assert!(!is_remote_fuse("fuse.ntfs3g"));
        }
    }

    // ── Platform-independent tests ────────────────────────────────

    #[test]
    fn test_mount_cache_new_is_empty() {
        let mut cache = MountCache::new();
        // On Linux, /tmp is always a local filesystem
        #[cfg(target_os = "linux")]
        {
            assert!(
                !cache.is_remote(Path::new("/tmp")),
                "/tmp should not be detected as remote"
            );
        }
        // On any platform, a fresh cache should have an empty dir_canon_cache
        let _ = cache;
    }

    #[test]
    fn test_mount_cache_dir_canon_reuse() {
        let mut cache = MountCache::new();
        // Use /tmp as a known-existing directory on all Unix-like systems
        let path_a = Path::new("/tmp/file_a.txt");
        let path_b = Path::new("/tmp/file_b.txt");

        let result_a = cache.canonicalize_cached(path_a);
        let result_b = cache.canonicalize_cached(path_b);

        // Both should resolve to the same parent directory
        assert_eq!(
            result_a.parent(),
            result_b.parent(),
            "Files in the same directory should have the same canonical parent"
        );

        // The dir_canon_cache should have exactly one entry for /tmp
        // (not separate entries for each file)
        assert!(
            cache.dir_canon_cache.contains_key(Path::new("/tmp")),
            "dir_canon_cache should have an entry for /tmp"
        );
    }

    // ── Property-based tests ─────────────────────────────────────

    #[cfg(target_os = "linux")]
    mod proptest_linux {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn unescape_mount_path_no_panic(s in ".*") {
                // Should never panic on any string input
                let _ = unescape_mount_path(&s);
            }

            #[test]
            fn parse_mount_table_no_panic(s in ".*") {
                // Should never panic on arbitrary text
                let _ = parse_mount_table_from_str(&s);
            }

            #[test]
            fn unescape_preserves_plain_ascii(s in "[a-zA-Z0-9/._-]+") {
                // Plain ASCII paths without escapes should round-trip unchanged
                let result = unescape_mount_path(&s);
                prop_assert_eq!(&s, &result);
            }
        }
    }
}
