//! Dolby Vision sample entry repair for stream-copied MP4 files.
//!
//! ffmpeg's mov muxer has no tag for `dvhe`, so a copy of a Dolby Vision
//! file whose parameter sets are in-band comes out as `hev1` (or `hvc1`)
//! with the `dvcC` record intact. Players that choose Dolby Vision decoding
//! from the sample entry then treat the file as plain HEVC, which is wrong
//! for profile 5. The two entries differ only in their four-character code,
//! so the code is rewritten in place and nothing else in the file moves.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

/// Fixed fields of a VisualSampleEntry that precede its child boxes.
const VISUAL_ENTRY_FIELDS: u64 = 78;

/// Boxes between the file and the sample description.
const CONTAINERS: [&[u8; 4]; 5] = [b"moov", b"trak", b"mdia", b"minf", b"stbl"];

struct BoxHeader {
    kind: [u8; 4],
    /// Offset of the box's first byte.
    start: u64,
    /// Offset of its first content byte.
    content: u64,
    /// Offset one past its last byte.
    end: u64,
}

/// The box header at `pos`, or `None` where the data runs out or the sizes
/// are not self-consistent (the walk then stops rather than guesses).
fn read_header<F: Read + Seek>(
    f: &mut F,
    pos: u64,
    limit: u64,
) -> Result<Option<BoxHeader>, String> {
    if pos.checked_add(8).is_none_or(|e| e > limit) {
        return Ok(None);
    }
    f.seek(SeekFrom::Start(pos)).map_err(|e| e.to_string())?;
    let mut h = [0u8; 8];
    f.read_exact(&mut h).map_err(|e| e.to_string())?;
    let size32 = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) as u64;
    let kind = [h[4], h[5], h[6], h[7]];
    let (header_len, size) = match size32 {
        0 => (8, limit - pos),
        1 => {
            if pos + 16 > limit {
                return Ok(None);
            }
            let mut big = [0u8; 8];
            f.read_exact(&mut big).map_err(|e| e.to_string())?;
            (16, u64::from_be_bytes(big))
        }
        n => (8, n),
    };
    if size < header_len || pos.checked_add(size).is_none_or(|e| e > limit) {
        return Ok(None);
    }
    Ok(Some(BoxHeader {
        kind,
        start: pos,
        content: pos + header_len,
        end: pos + size,
    }))
}

/// Whether the sample entry's child boxes include a Dolby Vision record.
fn has_dolby_vision_box<F: Read + Seek>(f: &mut F, entry: &BoxHeader) -> Result<bool, String> {
    let mut pos = entry.content + VISUAL_ENTRY_FIELDS;
    while let Some(child) = read_header(f, pos, entry.end)? {
        if child.kind == *b"dvcC" || child.kind == *b"dvvC" {
            return Ok(true);
        }
        pos = child.end;
    }
    Ok(false)
}

fn dolby_vision_code(kind: &[u8; 4]) -> Option<&'static [u8; 4]> {
    match kind {
        b"hev1" => Some(b"dvhe"),
        b"hvc1" => Some(b"dvh1"),
        _ => None,
    }
}

fn mark_in_stsd<F: Read + Write + Seek>(f: &mut F, stsd: &BoxHeader) -> Result<u32, String> {
    let mut changed = 0;
    // Version and flags, then the entry count.
    let mut pos = stsd.content + 8;
    while let Some(entry) = read_header(f, pos, stsd.end)? {
        if let Some(code) = dolby_vision_code(&entry.kind) {
            if has_dolby_vision_box(f, &entry)? {
                f.seek(SeekFrom::Start(entry.start + 4))
                    .map_err(|e| e.to_string())?;
                f.write_all(code).map_err(|e| e.to_string())?;
                changed += 1;
            }
        }
        pos = entry.end;
    }
    Ok(changed)
}

fn walk<F: Read + Write + Seek>(f: &mut F, start: u64, end: u64) -> Result<u32, String> {
    let mut changed = 0;
    let mut pos = start;
    while let Some(b) = read_header(f, pos, end)? {
        if b.kind == *b"stsd" {
            changed += mark_in_stsd(f, &b)?;
        } else if CONTAINERS.iter().any(|c| **c == b.kind) {
            changed += walk(f, b.content, b.end)?;
        }
        pos = b.end;
    }
    Ok(changed)
}

/// Rewrite `hev1` to `dvhe` and `hvc1` to `dvh1` in every sample entry that
/// carries a Dolby Vision configuration box (`dvcC` or `dvvC`). Returns how
/// many entries changed. A file that is not a well-formed MP4 is left as it
/// is.
pub fn mark_dolby_vision_entries<F: Read + Write + Seek>(file: &mut F) -> Result<u32, String> {
    let len = file.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
    walk(file, 0, len)
}

/// `mark_dolby_vision_entries` on the file at `path`.
pub fn mark_dolby_vision_file(path: &Path) -> Result<u32, String> {
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| format!("could not open {}: {e}", path.display()))?;
    mark_dolby_vision_entries(&mut f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    }

    /// A sample entry: 78 bytes of visual entry fields, then child boxes.
    fn entry(kind: &[u8; 4], children: &[Vec<u8>]) -> Vec<u8> {
        let mut body = vec![0u8; 78];
        for c in children {
            body.extend_from_slice(c);
        }
        boxed(kind, &body)
    }

    /// ftyp, mdat, then moov/trak/mdia/minf/stbl/stsd holding `entries`.
    fn mp4_with(entries: &[Vec<u8>]) -> Vec<u8> {
        let mut stsd_body = vec![0, 0, 0, 0];
        stsd_body.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        for e in entries {
            stsd_body.extend_from_slice(e);
        }
        let stsd = boxed(b"stsd", &stsd_body);
        let stbl = boxed(b"stbl", &[boxed(b"stts", &[0; 8]), stsd].concat());
        let minf = boxed(b"minf", &stbl);
        let mdia = boxed(b"mdia", &minf);
        let trak = boxed(b"trak", &[boxed(b"tkhd", &[0; 4]), mdia].concat());
        let moov = boxed(b"moov", &[boxed(b"mvhd", &[0; 4]), trak].concat());
        [
            boxed(b"ftyp", b"isom\0\0\0\0isom"),
            boxed(b"mdat", &[7u8; 16]),
            moov,
        ]
        .concat()
    }

    fn hvcc() -> Vec<u8> {
        boxed(b"hvcC", &[1; 23])
    }
    fn dvcc() -> Vec<u8> {
        boxed(
            b"dvcC",
            &[
                1, 0, 10, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            ],
        )
    }

    fn run(bytes: Vec<u8>) -> (Result<u32, String>, Vec<u8>) {
        let mut c = Cursor::new(bytes);
        let r = mark_dolby_vision_entries(&mut c);
        (r, c.into_inner())
    }

    fn swapped(mut bytes: Vec<u8>, from: &[u8; 4], to: &[u8; 4]) -> Vec<u8> {
        let at = bytes.windows(4).position(|w| w == from).unwrap();
        bytes[at..at + 4].copy_from_slice(to);
        bytes
    }

    #[test]
    fn hev1_with_dvcc_becomes_dvhe() {
        let src = mp4_with(&[entry(b"hev1", &[hvcc(), dvcc()])]);
        let (r, out) = run(src.clone());
        assert_eq!(r, Ok(1));
        assert_eq!(out, swapped(src, b"hev1", b"dvhe"));
    }

    #[test]
    fn hvc1_with_dvcc_becomes_dvh1() {
        let src = mp4_with(&[entry(b"hvc1", &[hvcc(), dvcc()])]);
        let (r, out) = run(src.clone());
        assert_eq!(r, Ok(1));
        assert_eq!(out, swapped(src, b"hvc1", b"dvh1"));
    }

    #[test]
    fn dvvc_counts_as_a_dolby_vision_box() {
        let dvvc = boxed(b"dvvC", &[0; 24]);
        let src = mp4_with(&[entry(b"hev1", &[hvcc(), dvvc])]);
        assert_eq!(run(src).0, Ok(1));
    }

    #[test]
    fn plain_hevc_without_dv_box_is_untouched() {
        let src = mp4_with(&[entry(b"hev1", &[hvcc()])]);
        let (r, out) = run(src.clone());
        assert_eq!(r, Ok(0));
        assert_eq!(out, src);
    }

    #[test]
    fn entries_already_dolby_vision_are_untouched() {
        for kind in [b"dvhe", b"dvh1"] {
            let src = mp4_with(&[entry(kind, &[hvcc(), dvcc()])]);
            let (r, out) = run(src.clone());
            assert_eq!(r, Ok(0));
            assert_eq!(out, src);
        }
    }

    #[test]
    fn other_codecs_are_untouched() {
        let src = mp4_with(&[entry(b"avc1", &[dvcc()])]);
        let (r, out) = run(src.clone());
        assert_eq!(r, Ok(0));
        assert_eq!(out, src);
    }

    #[test]
    fn only_the_four_character_code_changes() {
        let src = mp4_with(&[entry(b"hev1", &[hvcc(), dvcc()])]);
        let (_, out) = run(src.clone());
        assert_eq!(out.len(), src.len());
        let diff = src.iter().zip(&out).filter(|(a, b)| a != b).count();
        assert!(diff <= 4);
    }

    #[test]
    fn moov_before_mdat_and_64_bit_mdat_size_are_walked() {
        let mut src = mp4_with(&[entry(b"hev1", &[hvcc(), dvcc()])]);
        // Move moov in front of mdat, and give mdat a 64-bit size field.
        let ftyp_len = 8 + 12;
        let mdat_len = 8 + 16;
        let moov = src[ftyp_len + mdat_len..].to_vec();
        let mut big_mdat = 1u32.to_be_bytes().to_vec();
        big_mdat.extend_from_slice(b"mdat");
        big_mdat.extend_from_slice(&((16 + 16) as u64).to_be_bytes());
        big_mdat.extend_from_slice(&[7u8; 16]);
        src = [src[..ftyp_len].to_vec(), moov, big_mdat].concat();
        let (r, out) = run(src.clone());
        assert_eq!(r, Ok(1));
        assert_eq!(out, swapped(src, b"hev1", b"dvhe"));
    }

    #[test]
    fn a_second_run_changes_nothing() {
        let src = mp4_with(&[entry(b"hev1", &[hvcc(), dvcc()])]);
        let (_, once) = run(src);
        let (r, twice) = run(once.clone());
        assert_eq!(r, Ok(0));
        assert_eq!(twice, once);
    }

    #[test]
    fn truncated_or_garbage_input_is_an_error_or_a_no_op_never_a_panic() {
        let src = mp4_with(&[entry(b"hev1", &[hvcc(), dvcc()])]);
        for cut in [0, 4, 9, 40, src.len() - 30, src.len() - 1] {
            let (_, out) = run(src[..cut].to_vec());
            assert_eq!(out.len(), cut);
        }
        let (_, out) = run(vec![0xFF; 64]);
        assert_eq!(out, vec![0xFF; 64]);
    }

    #[test]
    fn missing_file_reports_the_path() {
        let e = mark_dolby_vision_file(Path::new("/nonexistent/x.mp4")).unwrap_err();
        assert!(e.contains("/nonexistent/x.mp4"));
    }
}
