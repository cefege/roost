//! A channel's ordered history as `GetHistoryRecordsResp` carries it: the head
//! the retained window was cut from, the geometry its oldest record was
//! produced at, and the output/resize records in stream order. Ports the codec
//! half of `apps/worker/src/keeper/protocol-terminal.ts` (`encodeKeeperHistoryRecords`,
//! `decodeKeeperHistoryRecords`); the keeper encodes it, the worker's client
//! decodes it for an adoption or a core re-proof.

use crate::codec::{CodecError, KEEPER_MAX_HISTORY_RESIZE_RECORDS, check_dimension, read_sequence, read_u32};

/// The one format this payload has ever had; a reader refuses any other.
pub const HISTORY_FORMAT_VERSION: u8 = 1;
/// `[version:u8][head_seq:u64][base_cols:u32][base_rows:u32][count:u32]`.
pub const HISTORY_HEADER_BYTES: usize = 21;
/// `[tag:u8][len:u32]` before an output record's bytes.
const HISTORY_OUTPUT_HEADER_BYTES: usize = 5;
/// `[tag:u8][seq:u64][cols:u32][rows:u32]`.
const HISTORY_RESIZE_BYTES: usize = 17;
const HISTORY_OUTPUT_TAG: u8 = 1;
const HISTORY_RESIZE_TAG: u8 = 2;
/// Every retained marker plus the output run on each side of it.
pub const MAX_HISTORY_RECORDS: usize = KEEPER_MAX_HISTORY_RESIZE_RECORDS as usize * 2 + 1;
/// The retained ring plus its framing always fits well inside this.
pub const MAX_HISTORY_PAYLOAD_BYTES: usize = 2 * 1024 * 1024;
/// v2 encodes counters as JS numbers, so a head past 2^53-1 is not a head.
const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

/// One entry of a channel's ordered history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryRecord {
    /// A contiguous run of PTY output. Never empty on the wire.
    Output { bytes: Vec<u8> },
    /// The geometry the PTY took at this point of the stream, under the
    /// resize sequence that applied it (0 for the legacy unsequenced frame).
    Resize { seq: u64, cols: u16, rows: u16 },
}

/// A channel's ordered history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryRecords {
    /// Every raw byte the channel ever emitted, retained or not.
    pub head_seq: u64,
    /// The geometry immediately before the first retained record.
    pub base_cols: u16,
    pub base_rows: u16,
    pub records: Vec<HistoryRecord>,
}

impl HistoryRecords {
    /// v2's answer for a channel the keeper does not hold: nothing emitted, the
    /// default geometry, no records (`keeper-frame-handler.ts:510-515`).
    pub fn unknown_channel() -> Self {
        Self {
            head_seq: 0,
            base_cols: 80,
            base_rows: 24,
            records: Vec::new(),
        }
    }

    /// The retained output bytes, oldest first and contiguous.
    pub fn window(&self) -> Vec<u8> {
        let mut window = Vec::new();
        for record in &self.records {
            if let HistoryRecord::Output { bytes } = record {
                window.extend_from_slice(bytes);
            }
        }
        window
    }

    /// Encode under v2's bounds; a history that breaks one is a keeper bug and
    /// is refused rather than truncated.
    pub fn encode(&self) -> Result<Vec<u8>, CodecError> {
        if self.head_seq > MAX_SAFE_INTEGER {
            return Err(invalid("the head is past the largest exact counter"));
        }
        check_dimension(u32::from(self.base_cols))?;
        check_dimension(u32::from(self.base_rows))?;
        if self.records.len() > MAX_HISTORY_RECORDS {
            return Err(invalid("more records than the retention bound allows"));
        }
        let mut payload_bytes = HISTORY_HEADER_BYTES;
        let mut raw_bytes: u64 = 0;
        for record in &self.records {
            match record {
                HistoryRecord::Output { bytes } => {
                    if bytes.is_empty() {
                        return Err(invalid("an output record is empty"));
                    }
                    raw_bytes += bytes.len() as u64;
                    payload_bytes += HISTORY_OUTPUT_HEADER_BYTES + bytes.len();
                }
                HistoryRecord::Resize { seq, cols, rows } => {
                    if *seq > MAX_SAFE_INTEGER {
                        return Err(invalid("a resize sequence is past the largest exact counter"));
                    }
                    check_dimension(u32::from(*cols))?;
                    check_dimension(u32::from(*rows))?;
                    payload_bytes += HISTORY_RESIZE_BYTES;
                }
            }
            if payload_bytes > MAX_HISTORY_PAYLOAD_BYTES {
                return Err(invalid("the payload exceeds its bound"));
            }
        }
        if raw_bytes > self.head_seq {
            return Err(invalid("the retained output exceeds the head"));
        }
        let mut out = Vec::with_capacity(payload_bytes);
        out.push(HISTORY_FORMAT_VERSION);
        out.extend_from_slice(&self.head_seq.to_be_bytes());
        out.extend_from_slice(&u32::from(self.base_cols).to_be_bytes());
        out.extend_from_slice(&u32::from(self.base_rows).to_be_bytes());
        out.extend_from_slice(&(self.records.len() as u32).to_be_bytes());
        for record in &self.records {
            match record {
                HistoryRecord::Output { bytes } => {
                    out.push(HISTORY_OUTPUT_TAG);
                    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
                    out.extend_from_slice(bytes);
                }
                HistoryRecord::Resize { seq, cols, rows } => {
                    out.push(HISTORY_RESIZE_TAG);
                    out.extend_from_slice(&seq.to_be_bytes());
                    out.extend_from_slice(&u32::from(*cols).to_be_bytes());
                    out.extend_from_slice(&u32::from(*rows).to_be_bytes());
                }
            }
        }
        Ok(out)
    }

    /// Decode, refusing every shape v2's decoder refuses: a wrong version, an
    /// oversized payload, an empty or overlong output record, output that
    /// exceeds the head, an unknown tag, and trailing bytes.
    pub fn decode(payload: &[u8]) -> Result<Self, CodecError> {
        if payload.len() < HISTORY_HEADER_BYTES {
            return Err(CodecError::TruncatedPayload {
                name: "history records",
                offset: payload.len(),
            });
        }
        if payload.len() > MAX_HISTORY_PAYLOAD_BYTES {
            return Err(invalid("the payload exceeds its bound"));
        }
        if payload[0] != HISTORY_FORMAT_VERSION {
            return Err(invalid("an unknown history format version"));
        }
        let head_seq = read_sequence(payload, 1).ok_or_else(|| truncated(1))?;
        if head_seq > MAX_SAFE_INTEGER {
            return Err(invalid("the head is past the largest exact counter"));
        }
        let base_cols = dimension(payload, 9)?;
        let base_rows = dimension(payload, 13)?;
        let count = read_u32(payload, 17).ok_or_else(|| truncated(17))? as usize;
        if count > MAX_HISTORY_RECORDS {
            return Err(invalid("more records than the retention bound allows"));
        }
        let mut records = Vec::with_capacity(count);
        let mut raw_bytes: u64 = 0;
        let mut offset = HISTORY_HEADER_BYTES;
        for _ in 0..count {
            match payload.get(offset) {
                Some(&HISTORY_OUTPUT_TAG) => {
                    let length = read_u32(payload, offset + 1).ok_or_else(|| truncated(offset))? as usize;
                    let start = offset + HISTORY_OUTPUT_HEADER_BYTES;
                    let bytes = payload
                        .get(start..start + length)
                        .filter(|bytes| !bytes.is_empty())
                        .ok_or_else(|| invalid("an output record is empty or runs past the payload"))?;
                    raw_bytes += length as u64;
                    if raw_bytes > head_seq {
                        return Err(invalid("the retained output exceeds the head"));
                    }
                    records.push(HistoryRecord::Output { bytes: bytes.to_vec() });
                    offset = start + length;
                }
                Some(&HISTORY_RESIZE_TAG) => {
                    if offset + HISTORY_RESIZE_BYTES > payload.len() {
                        return Err(truncated(offset));
                    }
                    let seq = read_sequence(payload, offset + 1).ok_or_else(|| truncated(offset))?;
                    if seq > MAX_SAFE_INTEGER {
                        return Err(invalid("a resize sequence is past the largest exact counter"));
                    }
                    let cols = dimension(payload, offset + 9)?;
                    let rows = dimension(payload, offset + 13)?;
                    records.push(HistoryRecord::Resize { seq, cols, rows });
                    offset += HISTORY_RESIZE_BYTES;
                }
                Some(other) => return Err(invalid_owned(format!("unknown record tag {other}"))),
                None => return Err(truncated(offset)),
            }
        }
        if offset != payload.len() {
            return Err(invalid("bytes trail the last record"));
        }
        Ok(Self {
            head_seq,
            base_cols,
            base_rows,
            records,
        })
    }
}

fn dimension(payload: &[u8], offset: usize) -> Result<u16, CodecError> {
    let value = read_u32(payload, offset).ok_or_else(|| truncated(offset))?;
    Ok(check_dimension(value)? as u16)
}

fn truncated(offset: usize) -> CodecError {
    CodecError::TruncatedPayload {
        name: "history records",
        offset,
    }
}

fn invalid(reason: &str) -> CodecError {
    invalid_owned(reason.to_owned())
}

fn invalid_owned(reason: String) -> CodecError {
    CodecError::BadJson {
        name: "history records",
        reason,
    }
}
