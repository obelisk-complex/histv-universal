//! Remote mount detection for local staging decisions.
//!
//! Detects whether a file resides on a network-mounted filesystem (NFS, CIFS,
//! sshfs, etc.) so the CLI can stage it locally before encoding. Detection is
//! platform-specific and cached for the lifetime of a batch.

use std::path::{Path, PathBuf};

#[cfg(target_os = "windows")]
use std::collections::HashMap;

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
pub struct MountCache {
    /// Parsed mount entries, sorted by mount point length (longest first)
    /// for correct longest-prefix matching.
    #[cfg(unix)]
    entries: Option<Vec<MountEntry>>,

    /// Windows-only: cached drive letter -> is_remote results.
    #[cfg(target_os = "windows")]
    drive_cache: HashMap<char, bool>,
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
    "nfs", "nfs4", "cifs", "smb", "smb2", "smb3",
    "fuse.sshfs", "fuse.rclone", "fuse.s3fs",
    "9p", "afs",
];

/// Known remote filesystem types on macOS.
#[cfg(target_os = "macos")]
const REMOTE_FS_TYPES: &[&str] = &[
    "nfs", "smbfs", "afpfs", "webdav",
    "fuse.sshfs", "fuse.rclone", "fuse.s3fs",
];

impl MountCache {
    pub fn new() -> Self {
        Self {
            #[cfg(unix)]
            entries: None,
            #[cfg(target_os = "windows")]
            drive_cache: HashMap::new(),
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
    pub fn mount_info(&mut self, path: &Path) -> Option<MountInfo> {
        // Canonicalise the path to resolve symlinks before matching
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

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
                mount_point: PathBuf::from(clean_str.split('\\').take(4).collect::<Vec<_>>().join("\\")),
                fs_type: "UNC".to_string(),
                is_remote: true,
            });
        }

        // Drive letter paths: check GetDriveTypeW
        let drive_letter = clean_str.chars().next()?;
        if !drive_letter.is_ascii_alphabetic() {
            return None;
        }

        let is_remote = self.drive_cache.entry(drive_letter.to_ascii_uppercase()).or_insert_with(|| {
            check_drive_type_windows(drive_letter)
        });

        Some(MountInfo {
            mount_point: PathBuf::from(format!("{}:\\", drive_letter.to_ascii_uppercase())),
            fs_type: if *is_remote { "network".to_string() } else { "local".to_string() },
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
                || fs_type.starts_with("fuse.") && is_remote_fuse(&fs_type);
            Some(MountEntry {
                mount_point: PathBuf::from(mount_point),
                fs_type,
                is_remote,
            })
        })
        .collect();

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

/// Unescape octal sequences in /proc/mounts paths (e.g. \040 for space).
#[cfg(target_os = "linux")]
fn unescape_mount_path(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            // Try to read 3 octal digits
            let mut octal = String::new();
            for _ in 0..3 {
                if let Some(&next) = chars.as_str().as_bytes().first() {
                    if next >= b'0' && next <= b'7' {
                        octal.push(chars.next().unwrap());
                    } else {
                        break;
                    }
                }
            }
            if octal.len() == 3 {
                if let Ok(byte) = u8::from_str_radix(&octal, 8) {
                    result.push(byte as char);
                } else {
                    result.push('\\');
                    result.push_str(&octal);
                }
            } else {
                result.push('\\');
                result.push_str(&octal);
            }
        } else {
            result.push(c);
        }
    }
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
            let is_remote = REMOTE_FS_TYPES.iter().any(|rt| *rt == fs_type)
                || fs_type.starts_with("fuse.");
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