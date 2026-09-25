//! A channel's ordered history: the output and geometry records a fresh
//! worker replays to rebuild a channel it did not spawn. Owned by the keeper
//! and the worker's client.
//!
//! The contract is `protocol/spec/keeper.md`. The point of the ordering is
//! that a late joiner learns not just the bytes but the sequence at which the
//! grid must be rebuilt, so a replay cannot produce a plausible wrong screen.

use crate::codec::{
    CodecError, KEEPER_MAX_HISTORY_RESIZE_RECORDS, check_dimension, read_sequence, read_u32,
    write_sequence,
};

/// One entry of a channel's ordered history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryRecord {
    /// PTY output, with the sequence that orders it.
    Output { seq: u64, bytes: Vec<u8> },
    /// A geometry change, retained so a late joiner learns the sequence
    /// boundary at which the grid must be rebuilt.
    Resize { seq: u64, cols: u16, rows: u16 },
}

impl HistoryRecord {
    /// `[tag:u8][seq:u64][body]`, so a reader can tell the two apart without
    /// knowing which sequence numbers are present.
    pub fn encode(&self) -> Result<Vec<u8>, CodecError> {
        let mut out = Vec::new();
        match self {
            HistoryRecord::Output { seq, bytes } => {
                out.push(1);
                write_sequence(&mut out, *seq);
                out.extend_from_slice(bytes);
            }
            HistoryRecord::Resize { seq, cols, rows } => {
                out.push(2);
                write_sequence(&mut out, *seq);
                // u32, not u16: this is the same shape as a ResizeAck, so one
                // reader serves both. Encoding u16 here while the decoder read
                // u32 was a live bug the round-trip test caught.
                out.extend_from_slice(&(*cols as u32).to_be_bytes());
                out.extend_from_slice(&(*rows as u32).to_be_bytes());
            }
        }
        Ok(out)
    }

    pub fn decode(payload: &[u8]) -> Result<Self, CodecError> {
        let tag = *payload.first().ok_or(CodecError::TruncatedPayload {
            name: "history",
            offset: 0,
        })?;
        let seq = read_sequence(payload, 1).ok_or(CodecError::TruncatedPayload {
            name: "history",
            offset: 1,
        })?;
        match tag {
            1 => Ok(HistoryRecord::Output {
                seq,
                bytes: payload[9..].to_vec(),
            }),
            2 => {
                let cols = read_u32(payload, 9).ok_or(CodecError::TruncatedPayload {
                    name: "history",
                    offset: 9,
                })?;
                let rows = read_u32(payload, 13).ok_or(CodecError::TruncatedPayload {
                    name: "history",
                    offset: 13,
                })?;
                Ok(HistoryRecord::Resize {
                    seq,
                    cols: check_dimension(cols)? as u16,
                    rows: check_dimension(rows)? as u16,
                })
            }
            other => Err(CodecError::BadJson {
                name: "history",
                reason: format!("unknown record tag {other}"),
            }),
        }
    }
}

/// A channel's ordered history, as `GetHistoryRecordsResp` carries it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistoryRecords {
    pub records: Vec<HistoryRecord>,
}

impl HistoryRecords {
    /// Whether the resize records are within the retention bound. Exceeding it
    /// is not an encoding error: the keeper answers with what it still has.
    pub fn resize_record_count(&self) -> u32 {
        self.records
            .iter()
            .filter(|r| matches!(r, HistoryRecord::Resize { .. }))
            .count() as u32
    }

    pub fn within_retention(&self) -> bool {
        self.resize_record_count() <= KEEPER_MAX_HISTORY_RESIZE_RECORDS
    }

    /// Encode as `[count:u32]` then each record, each self-delimiting by its
    /// own encoded length so a reader never has to guess a record's extent.
    pub fn encode(&self) -> Result<Vec<u8>, CodecError> {
        let mut out = Vec::new();
        out.extend_from_slice(&(self.records.len() as u32).to_be_bytes());
        for record in &self.records {
            let body = record.encode()?;
            out.extend_from_slice(&(body.len() as u32).to_be_bytes());
            out.extend_from_slice(&body);
        }
        Ok(out)
    }

    pub fn decode(payload: &[u8]) -> Result<Self, CodecError> {
        let count = read_u32(payload, 0).ok_or(CodecError::TruncatedPayload {
            name: "history records",
            offset: 0,
        })?;
        let mut records = Vec::with_capacity(count.min(1024) as usize);
        let mut offset = 4;
        for _ in 0..count {
            let len = read_u32(payload, offset).ok_or(CodecError::TruncatedPayload {
                name: "history records",
                offset,
            })? as usize;
            let start = offset + 4;
            let end = start.checked_add(len).ok_or(CodecError::LengthMismatch {
                claimed: len as u32,
                actual: payload.len(),
            })?;
            let body = payload.get(start..end).ok_or(CodecError::LengthMismatch {
                claimed: len as u32,
                actual: payload.len().saturating_sub(start),
            })?;
            records.push(HistoryRecord::decode(body)?);
            offset = end;
        }
        Ok(Self { records })
    }
}
