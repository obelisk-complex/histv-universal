//! Shared HEVC Annex B bitstream utilities for DV and HDR10+ pipelines.
//!
//! Provides a streaming NAL unit reader/writer that processes bitstreams
//! without loading them entirely into memory.

use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::path::Path;

/// Annex B start code (4-byte form).
pub const START_CODE: [u8; 4] = [0x00, 0x00, 0x00, 0x01];

/// HEVC NAL unit types.
pub const HEVC_NAL_UNSPEC62: u8 = 62; // Dolby Vision RPU
pub const HEVC_NAL_SEI_PREFIX: u8 = 39;

/// A parsed NAL unit from an Annex B bitstream.
pub struct NalUnit {
    /// NAL unit type (6 bits from first byte of the NAL header).
    pub nal_type: u8,
    /// Raw NAL unit data (excluding the start code, including the NAL header).
    pub data: Vec<u8>,
}

impl NalUnit {
    /// Whether this is a VCL NAL unit (types 0-31).
    pub fn is_vcl(&self) -> bool {
        self.nal_type <= 31
    }

    /// Whether this is the first slice segment of a new picture.
    /// Checks the `first_slice_segment_in_pic_flag` (MSB of the byte
    /// after the 2-byte NAL header).
    pub fn is_first_slice_of_picture(&self) -> bool {
        self.is_vcl() && self.data.len() > 2 && (self.data[2] & 0x80) != 0
    }
}

// ── Streaming NAL Reader ──────────────────────────────────────────

/// Maximum allowed NAL unit size (256 MB). A legitimate 4K HEVC slice is
/// at most a few MB. If we accumulate beyond this without finding the next
/// start code, the bitstream is malformed. Without this guard, a truncated
/// or corrupted bitstream could cause the reader to buffer the entire
/// remaining file (potentially tens of GB) looking for a start code.
const MAX_NAL_SIZE: usize = 256 * 1024 * 1024;

/// Reads NAL units one at a time from an Annex B bitstream without
/// loading the entire file into memory.
///
/// Uses a state machine that scans for start codes (0x000001 or 0x00000001)
/// in a buffered reader, accumulating each NAL unit's bytes.
pub struct NalReader<R: Read> {
    reader: BufReader<R>,
    /// Accumulator for the current NAL unit being read.
    buf: Vec<u8>,
    /// Number of consecutive zero bytes seen.
    zeros: u32,
    /// Whether we've found the first start code yet.
    started: bool,
    /// Whether the underlying reader is exhausted.
    eof: bool,
}

impl<R: Read> NalReader<R> {
    pub fn new(reader: R) -> Self {
        Self {
            // 2 MB read buffer — 4K HEVC slices can be several MB each
            reader: BufReader::with_capacity(2 * 1024 * 1024, reader),
            buf: Vec::with_capacity(256 * 1024),
            zeros: 0,
            started: false,
            eof: false,
        }
    }

    /// Read the next NAL unit from the stream.
    /// Returns `None` at end of stream.
    pub fn next_nalu(&mut self) -> io::Result<Option<NalUnit>> {
        if self.eof {
            return Ok(None);
        }

        loop {
            // Scan the buffer for start codes. When a start code is found
            // that terminates a NAL we've been accumulating, break out of
            // the inner loop so the fill_buf borrow is released before we
            // call flush_current().
            let mut nalu_complete = false;
            let mut sc_trim = 0usize;
            let consumed;

            {
                let available = self.reader.fill_buf()?;
                if available.is_empty() {
                    self.eof = true;
                    consumed = 0;
                    nalu_complete = self.started && !self.buf.is_empty();
                    // Will flush below after the borrow is released
                } else {
                    let len = available.len();
                    let mut pos = len; // default: consume everything

                    for i in 0..len {
                        let byte = available[i];

                        if byte == 0 {
                            self.zeros += 1;
                            if self.started {
                                self.buf.push(byte);
                            }
                            continue;
                        }

                        if byte == 1 && self.zeros >= 2 {
                            // Found a start code
                            let sc_zeros = self.zeros.min(3) as usize;

                            if self.started {
                                // Remove trailing zeros (they're the start code, not NAL data)
                                sc_trim = sc_zeros;
                                nalu_complete = true;
                                pos = i + 1;
                                self.zeros = 0;
                                break;
                            }

                            // First start code found
                            self.started = true;
                            self.zeros = 0;
                            continue;
                        }

                        // Regular byte
                        if self.started {
                            self.buf.push(byte);
                        }
                        self.zeros = 0;
                    }

                    consumed = pos;
                }
            }
            // fill_buf borrow released here

            if consumed > 0 {
                self.reader.consume(consumed);
            }

            if self.eof {
                return Ok(self.flush_current());
            }

            // Guard against malformed bitstreams: if the accumulator has grown
            // beyond any sane NAL size without finding the next start code,
            // the bitstream is corrupt. Bail instead of buffering the entire file.
            if !nalu_complete && self.buf.len() > MAX_NAL_SIZE {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "NAL unit exceeds {} MB without a terminating start code — \
                         bitstream appears malformed",
                        MAX_NAL_SIZE / (1024 * 1024),
                    ),
                ));
            }

            if nalu_complete {
                let trim = self.buf.len().saturating_sub(sc_trim);
                self.buf.truncate(trim);

                let nalu = self.flush_current();
                self.started = true;

                if let Some(n) = nalu {
                    return Ok(Some(n));
                }
                // Empty NAL (shouldn't happen), continue
            }
        }
    }

    /// Finalize the current NAL unit buffer into a NalUnit.
    fn flush_current(&mut self) -> Option<NalUnit> {
        if self.buf.is_empty() {
            return None;
        }

        let data = std::mem::take(&mut self.buf);
        let nal_type = (data[0] >> 1) & 0x3F;
        Some(NalUnit { nal_type, data })
    }
}

// ── Streaming NAL Writer ──────────────────────────────────────────

/// Writes NAL units to an Annex B bitstream with buffered I/O.
pub struct NalWriter<W: Write> {
    writer: BufWriter<W>,
}

impl<W: Write> NalWriter<W> {
    pub fn new(writer: W) -> Self {
        Self {
            writer: BufWriter::with_capacity(2 * 1024 * 1024, writer),
        }
    }

    /// Write a NAL unit with a 4-byte start code prefix.
    pub fn write_nalu(&mut self, data: &[u8]) -> io::Result<()> {
        self.writer.write_all(&START_CODE)?;
        self.writer.write_all(data)?;
        Ok(())
    }

    /// Flush the underlying writer.
    pub fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

// ── Convenience functions ─────────────────────────────────────────

/// Extract specific NAL units from a bitstream file using streaming I/O.
/// The `filter` closure receives each NAL unit and returns `true` to collect it.
/// Only the collected NAL data is held in memory.
pub fn extract_nalus_filtered<F>(path: &Path, mut filter: F) -> io::Result<Vec<Vec<u8>>>
where
    F: FnMut(&NalUnit) -> bool,
{
    let file = std::fs::File::open(path)?;
    let mut reader = NalReader::new(file);
    let mut collected = Vec::new();

    while let Some(nalu) = reader.next_nalu()? {
        if filter(&nalu) {
            collected.push(nalu.data);
        }
    }

    Ok(collected)
}

/// Process a bitstream file: for each NAL unit, call `processor` which can
/// emit zero or more NAL units to the writer. Streams through without
/// loading the full file.
pub fn transform_bitstream<F>(
    input_path: &Path,
    output_path: &Path,
    mut processor: F,
) -> io::Result<()>
where
    F: FnMut(&NalUnit, &mut NalWriter<std::fs::File>) -> io::Result<()>,
{
    let in_file = std::fs::File::open(input_path)?;
    let out_file = std::fs::File::create(output_path)?;
    let mut reader = NalReader::new(in_file);
    let mut writer = NalWriter::new(out_file);

    while let Some(nalu) = reader.next_nalu()? {
        processor(&nalu, &mut writer)?;
    }

    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal Annex B bitstream from raw NAL payloads.
    fn annex_b(nalus: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for nalu in nalus {
            out.extend_from_slice(&START_CODE);
            out.extend_from_slice(nalu);
        }
        out
    }

    #[test]
    fn nal_reader_parses_two_nalus() {
        // NAL type is (first_byte >> 1) & 0x3F.
        // Type 1 (coded slice): 0x02, 0x01 header → (0x02 >> 1) & 0x3F = 1
        // Type 39 (SEI_PREFIX): 0x4E, 0x01 header → (0x4E >> 1) & 0x3F = 39
        let nalu1 = &[0x02, 0x01, 0xAA, 0xBB]; // VCL type 1
        let nalu2 = &[0x4E, 0x01, 0xCC]; // SEI_PREFIX type 39
        let stream = annex_b(&[nalu1, nalu2]);

        let mut reader = NalReader::new(std::io::Cursor::new(&stream));

        let n1 = reader.next_nalu().unwrap().unwrap();
        assert_eq!(n1.nal_type, 1);
        assert_eq!(n1.data, nalu1);
        assert!(n1.is_vcl());

        let n2 = reader.next_nalu().unwrap().unwrap();
        assert_eq!(n2.nal_type, HEVC_NAL_SEI_PREFIX);
        assert_eq!(n2.data, &[0x4E, 0x01, 0xCC]);
        assert!(!n2.is_vcl());

        assert!(reader.next_nalu().unwrap().is_none());
    }

    #[test]
    fn nal_reader_handles_three_byte_start_code() {
        // 3-byte start code: 00 00 01
        let mut stream = vec![0x00, 0x00, 0x01, 0x02, 0x01, 0xFF];
        // Second NAL with 4-byte start code
        stream.extend_from_slice(&START_CODE);
        stream.extend_from_slice(&[0x4E, 0x01]);

        let mut reader = NalReader::new(std::io::Cursor::new(&stream));

        let n1 = reader.next_nalu().unwrap().unwrap();
        assert_eq!(n1.nal_type, 1);

        let n2 = reader.next_nalu().unwrap().unwrap();
        assert_eq!(n2.nal_type, HEVC_NAL_SEI_PREFIX);

        assert!(reader.next_nalu().unwrap().is_none());
    }

    #[test]
    fn nal_reader_empty_stream() {
        let mut reader = NalReader::new(std::io::Cursor::new(&[]));
        assert!(reader.next_nalu().unwrap().is_none());
    }

    #[test]
    fn nal_reader_single_nalu_no_trailing_start_code() {
        let stream = annex_b(&[&[0x02, 0x01, 0xDE, 0xAD]]);
        let mut reader = NalReader::new(std::io::Cursor::new(&stream));

        let n = reader.next_nalu().unwrap().unwrap();
        assert_eq!(n.data, &[0x02, 0x01, 0xDE, 0xAD]);
        assert!(reader.next_nalu().unwrap().is_none());
    }

    #[test]
    fn is_first_slice_of_picture() {
        // VCL NAL (type 1) with first_slice_segment_in_pic_flag set (MSB of byte 2)
        let n = NalUnit {
            nal_type: 1,
            data: vec![0x02, 0x01, 0x80],
        };
        assert!(n.is_first_slice_of_picture());

        // Same but flag not set
        let n2 = NalUnit {
            nal_type: 1,
            data: vec![0x02, 0x01, 0x00],
        };
        assert!(!n2.is_first_slice_of_picture());

        // Non-VCL NAL — never first slice regardless of flag
        let n3 = NalUnit {
            nal_type: 39,
            data: vec![0x4E, 0x01, 0x80],
        };
        assert!(!n3.is_first_slice_of_picture());
    }

    #[test]
    fn nal_writer_round_trip() {
        let nalu1 = vec![0x02, 0x01, 0xAA];
        let nalu2 = vec![0x4E, 0x01, 0xBB, 0xCC];

        let mut buf = Vec::new();
        {
            let mut writer = NalWriter::new(&mut buf);
            writer.write_nalu(&nalu1).unwrap();
            writer.write_nalu(&nalu2).unwrap();
            writer.flush().unwrap();
        }

        // Read them back
        let mut reader = NalReader::new(std::io::Cursor::new(&buf));
        let n1 = reader.next_nalu().unwrap().unwrap();
        assert_eq!(n1.data, nalu1);
        let n2 = reader.next_nalu().unwrap().unwrap();
        assert_eq!(n2.data, nalu2);
        assert!(reader.next_nalu().unwrap().is_none());
    }
}
