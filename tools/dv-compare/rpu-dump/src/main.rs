//! RPU helper for tools/dv-compare.
//!
//!   dvc-rpu dump FILE          one JSON object per Dolby Vision RPU NAL unit (type 62) in an Annex B stream
//!   dvc-rpu show FILE INDEX    full parse of one RPU, for the first differing frame in a report
//!   dvc-rpu gen OUT PROFILE FRAMES [VARIANT]
//!                              synthetic RPUs as Annex B NAL units, for the self-test fixtures
//!
//! `dump` expects access unit delimiters (the Python side inserts them with
//! `hevc_metadata=aud=insert`), so each RPU is attributed to the access unit, that is the
//! packet, it arrived in; the last line counts access units and RPUs.
use std::fmt::Write as _;
use std::{env, fs, process::ExitCode};

use anyhow::{bail, Context, Result};
use dolby_vision::rpu::dovi_rpu::DoviRpu;
use dolby_vision::rpu::extension_metadata::blocks::{
    ExtMetadataBlock, ExtMetadataBlockLevel1, ExtMetadataBlockLevel2, ExtMetadataBlockLevel4,
};
use dolby_vision::rpu::generate::{GenerateConfig, GenerateProfile, ShotFrameEdit, VideoShot};

const NAL_AUD: u8 = 35;
const NAL_RPU: u8 = 62;

/// Annex B NAL units (header included), start codes and trailing zero bytes removed.
fn split_nals(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = vec![];
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = vec![];
    for (k, &s) in starts.iter().enumerate() {
        let mut e = starts.get(k + 1).map_or(data.len(), |n| n - 3);
        while e > s && data[e - 1] == 0 {
            e -= 1;
        }
        if e > s {
            out.push(&data[s..e]);
        }
    }
    out
}

fn nal_type(nal: &[u8]) -> u8 {
    (nal[0] >> 1) & 0x3f
}

fn fnv1a(data: &[u8]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in data {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

fn json_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// One RPU as the crate parses it: mapping, display management data without its extension
/// blocks, and the extension blocks by level.
struct Parsed {
    profile: u8,
    header: String,
    mapping: String,
    dm: String,
    ext: Vec<(u8, Vec<String>)>,
}

fn parse(nal: &[u8]) -> Result<Parsed> {
    let rpu = DoviRpu::parse_unspec62_nalu(nal)?;
    let mut ext = vec![];
    let mut dm = String::from("none");
    if let Some(v) = &rpu.vdr_dm_data {
        for level in 1..=255u8 {
            let blocks: Vec<String> = v
                .level_blocks_iter(level)
                .map(|b| format!("{b:?}"))
                .collect();
            if !blocks.is_empty() {
                ext.push((level, blocks));
            }
        }
        let mut c = v.clone();
        c.cmv29_metadata = None;
        c.cmv40_metadata = None;
        dm = format!("{c:?}");
    }
    Ok(Parsed {
        profile: rpu.dovi_profile,
        header: format!("{:?}", rpu.header),
        mapping: format!("{:?}", rpu.rpu_data_mapping),
        dm,
        ext,
    })
}

fn dump(path: &str) -> Result<()> {
    let data = fs::read(path).with_context(|| format!("reading {path}"))?;
    let (mut aus, mut idx) = (0usize, 0usize);
    let mut out = String::new();
    for nal in split_nals(&data) {
        match nal_type(nal) {
            NAL_AUD => aus += 1,
            NAL_RPU => {
                let au = aus.saturating_sub(1);
                let head = format!(
                    "{{\"i\":{idx},\"au\":{au},\"nal_len\":{},\"nal_fnv\":\"{}\"",
                    nal.len(),
                    fnv1a(nal)
                );
                match parse(nal) {
                    Ok(p) => {
                        let ext = p
                            .ext
                            .iter()
                            .map(|(l, v)| {
                                let items: Vec<String> = v.iter().map(|s| json_str(s)).collect();
                                format!("\"{l}\":[{}]", items.join(","))
                            })
                            .collect::<Vec<_>>()
                            .join(",");
                        let _ = writeln!(
                            out,
                            "{head},\"ok\":true,\"profile\":{},\"header\":{},\"map_fnv\":\"{}\",\"map_len\":{},\
                             \"dm\":{},\"ext\":{{{ext}}}}}",
                            p.profile,
                            json_str(&p.header),
                            fnv1a(p.mapping.as_bytes()),
                            p.mapping.len(),
                            json_str(&p.dm),
                        );
                    }
                    Err(e) => {
                        let _ = writeln!(
                            out,
                            "{head},\"ok\":false,\"error\":{}}}",
                            json_str(&format!("{e:#}"))
                        );
                    }
                }
                idx += 1;
            }
            _ => {}
        }
    }
    let _ = writeln!(out, "{{\"summary\":true,\"aus\":{aus},\"rpus\":{idx}}}");
    print!("{out}");
    Ok(())
}

fn show(path: &str, index: usize) -> Result<()> {
    let data = fs::read(path)?;
    let nal = split_nals(&data)
        .into_iter()
        .filter(|n| nal_type(n) == NAL_RPU)
        .nth(index)
        .with_context(|| format!("no RPU number {index}"))?;
    let p = parse(nal)?;
    println!(
        "profile {}\nheader {}\nmapping {}\ndm {}",
        p.profile, p.header, p.mapping, p.dm
    );
    for (l, v) in p.ext {
        for b in v {
            println!("L{l} {b}");
        }
    }
    Ok(())
}

/// Synthetic RPUs: two shots, per-frame L1, per-shot L2 and L4. `variant` is `base` or
/// `l1@N` (L1 min_pq of frame N raised by one), `l4@N` (L4 anchor_pq of frame N raised by 3).
fn gen(out: &str, profile: &str, frames: usize, variant: &str) -> Result<()> {
    if frames < 4 {
        bail!("need at least 4 frames");
    }
    let l1 = |f: usize| {
        ExtMetadataBlock::Level1(ExtMetadataBlockLevel1 {
            min_pq: 20 + (f % 5) as u16,
            max_pq: 2300 + 7 * f as u16,
            avg_pq: 600 + 3 * f as u16,
        })
    };
    let l4 = |pq: u16| {
        ExtMetadataBlock::Level4(ExtMetadataBlockLevel4 {
            anchor_pq: pq,
            anchor_power: pq / 2,
        })
    };
    let l2 = |slope: u16| {
        ExtMetadataBlock::Level2(ExtMetadataBlockLevel2 {
            target_max_pq: 2081,
            trim_slope: slope,
            trim_offset: 2048,
            trim_power: 2048,
            trim_chroma_weight: 2048,
            trim_saturation_gain: 2048,
            ms_weight: 2048,
        })
    };
    let (kind, at) = match variant.split_once('@') {
        Some((k, n)) => (k, n.parse::<usize>().context("variant frame")?),
        None => (variant, usize::MAX),
    };
    if !matches!(kind, "base" | "l1" | "l4") {
        bail!("unknown variant {variant}");
    }
    let half = frames / 2;
    let mut shots = vec![];
    for (start, duration, slope, pq) in [
        (0, half, 2048u16, 2100u16),
        (half, frames - half, 2300, 2600),
    ] {
        let mut edits = vec![];
        for off in 0..duration {
            let f = start + off;
            let mut blocks = vec![l1(f)];
            if kind == "l1" && f == at {
                blocks = vec![ExtMetadataBlock::Level1(ExtMetadataBlockLevel1 {
                    min_pq: 20 + (f % 5) as u16 + 1,
                    max_pq: 2300 + 7 * f as u16,
                    avg_pq: 600 + 3 * f as u16,
                })];
            }
            edits.push(ShotFrameEdit {
                edit_offset: off,
                metadata_blocks: blocks,
            });
        }
        let mut shot = VideoShot {
            id: String::new(),
            start,
            duration,
            metadata_blocks: vec![l2(slope), l4(pq)],
            frame_edits: edits,
        };
        if kind == "l4" && at >= start && at < start + duration {
            let off = at - start;
            if let Some(e) = shot.frame_edits.iter_mut().find(|e| e.edit_offset == off) {
                e.metadata_blocks.push(l4(pq + 3)); // L4 is per shot; a frame edit overrides it for one frame
            }
        }
        shots.push(shot);
    }
    let cfg = GenerateConfig {
        profile: match profile {
            "5" => GenerateProfile::Profile5,
            "8.1" => GenerateProfile::Profile81,
            p => bail!("profile must be 5 or 8.1, got {p}"),
        },
        length: frames,
        shots,
        ..Default::default()
    };
    let mut rpus = cfg.generate_rpu_list()?;
    let mut bytes = vec![];
    for r in &mut rpus {
        bytes.extend_from_slice(&[0, 0, 0, 1]);
        bytes.extend_from_slice(&r.write_hevc_unspec62_nalu()?);
    }
    fs::write(out, bytes)?;
    Ok(())
}

fn main() -> ExitCode {
    let a: Vec<String> = env::args().collect();
    let r = match a.get(1).map(String::as_str) {
        Some("dump") if a.len() == 3 => dump(&a[2]),
        Some("show") if a.len() == 4 => a[3]
            .parse()
            .map_err(anyhow::Error::from)
            .and_then(|i| show(&a[2], i)),
        Some("gen") if a.len() >= 5 => a[4]
            .parse()
            .map_err(anyhow::Error::from)
            .and_then(|n| gen(&a[2], &a[3], n, a.get(5).map_or("base", String::as_str))),
        _ => {
            eprintln!(
                "usage: dvc-rpu dump FILE | show FILE INDEX | gen OUT PROFILE FRAMES [VARIANT]"
            );
            return ExitCode::from(2);
        }
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("dvc-rpu: {e:#}");
            ExitCode::from(3)
        }
    }
}
