//! The keeper's typed payloads: the JSON frames, the sequenced input, and the
//! sequenced geometry. Owned by the keeper and the worker client.
//! The contract is `protocol/spec/keeper.md`; the tags and the envelope live
//! in [`crate::codec`].

use serde::{Deserialize, Serialize};

use crate::codec::{
    CodecError, KEEPER_MAX_INPUT_BYTES, MuxFrame, check_dimension, read_sequence, read_u32,
    write_sequence,
};

/// A capability name. A feature the keeper did not negotiate must never be
/// used, so this is a closed set rather than a free string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeeperFeature {
    /// Ordered `Output`/`Resize` history replay.
    #[serde(rename = "ordered_history_v1")]
    OrderedHistory,
    /// Input is sequenced and acknowledged.
    #[serde(rename = "acknowledged_input_v1")]
    AcknowledgedInput,
    /// Resize is sequenced and acknowledged.
    #[serde(rename = "acknowledged_resize_v1")]
    AcknowledgedResize,
    /// Authoritative geometry recovery.
    #[serde(rename = "terminal_state_v1")]
    TerminalState,
}

impl KeeperFeature {
    /// Every feature this build understands.
    pub const SUPPORTED: [KeeperFeature; 4] = [
        KeeperFeature::OrderedHistory,
        KeeperFeature::AcknowledgedInput,
        KeeperFeature::AcknowledgedResize,
        KeeperFeature::TerminalState,
    ];

    /// The subset whose absence makes a surviving keeper unusable.
    ///
    /// Narrower than [`KeeperFeature::SUPPORTED`]: boot may retire an
    /// incompatible keeper only after both coordinator sessions and keeper
    /// bindings prove empty, and `TerminalState` is not required because
    /// resize can fall back to the last-written sequence probe.
    pub const REQUIRED: [KeeperFeature; 3] = [
        KeeperFeature::OrderedHistory,
        KeeperFeature::AcknowledgedInput,
        KeeperFeature::AcknowledgedResize,
    ];

    /// The feature this wire name denotes, or `None` when this build has never
    /// heard of it.
    pub fn from_wire_name(name: &str) -> Option<Self> {
        KeeperFeature::SUPPORTED
            .into_iter()
            .find(|feature| feature.wire_name() == name)
    }

    pub fn wire_name(self) -> &'static str {
        match self {
            KeeperFeature::OrderedHistory => "ordered_history_v1",
            KeeperFeature::AcknowledgedInput => "acknowledged_input_v1",
            KeeperFeature::AcknowledgedResize => "acknowledged_resize_v1",
            KeeperFeature::TerminalState => "terminal_state_v1",
        }
    }
}

/// The wire version. Bumped when a frame's shape or encoding changes, never
/// for an additive tag, which is feature-negotiated instead.
pub const KEEPER_PROTOCOL_VERSION: u32 = 3;

/// The process identity a keeper proves at `Hello`. An absent digest can never
/// prove equality, which is why this is carried separately from the negotiated
/// version rather than standing in for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeeperContractV1 {
    pub protocol_version: u32,
    /// The keeper binary's own version.
    pub keeper_version: String,
    /// A digest of the running binary, empty when it cannot be computed.
    pub implementation_digest: String,
}

/// The client's `Hello`.
///
/// `requested_features` is raw names, not the enum: a worker that asks for a
/// feature this build has never heard of must still be answered with the ones
/// it does. Typing it as the enum would fail the whole handshake on one
/// unrecognised name, which is exactly the additive negotiation the feature
/// list exists to support.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeeperHelloRequest {
    pub protocol_version: u32,
    pub requested_features: Vec<String>,
}

/// What the keeper observed about itself, so the worker can prove a binding
/// without trusting a pid it was handed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeeperObservation {
    pub contract: KeeperContractV1,
    pub live_channel_count: u32,
}

/// The keeper's `Hello` answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeeperHelloResponse {
    pub contract: KeeperContractV1,
    pub observation: KeeperObservation,
    pub features: Vec<KeeperFeature>,
}

/// Negotiate the capability set: what the client asked for that the keeper
/// also supports. Order follows the client, so both sides agree on the list.
pub fn negotiate_features(requested: &[String]) -> Vec<KeeperFeature> {
    // Order follows the client, so both sides agree on the negotiated list, and
    // a name this build has never heard of is skipped rather than failing the
    // handshake — which is the whole point of negotiating additively.
    let mut negotiated = Vec::new();
    for name in requested {
        let Some(feature) = KeeperFeature::from_wire_name(name) else {
            continue;
        };
        if !negotiated.contains(&feature) {
            negotiated.push(feature);
        }
    }
    negotiated
}

/// Sequenced input, so a write that is lost is distinguishable from one that
/// never happened. Without the sequence a short write, a full queue and a
/// dropped connection look identical and a keystroke vanishes silently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PtyInRequest {
    pub input_seq: u64,
    pub bytes: Vec<u8>,
}

impl PtyInRequest {
    /// Encode as `[input_seq:u64][bytes]`.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + self.bytes.len());
        write_sequence(&mut out, self.input_seq);
        out.extend_from_slice(&self.bytes);
        out
    }

    /// Decode `[input_seq:u64][bytes]`, rejecting an oversized payload.
    pub fn decode(payload: &[u8], name: &'static str) -> Result<Self, CodecError> {
        let Some(input_seq) = read_sequence(payload, 0) else {
            return Err(CodecError::TruncatedPayload { name, offset: 0 });
        };
        let bytes = payload[8..].to_vec();
        if bytes.len() as u64 > KEEPER_MAX_INPUT_BYTES as u64 {
            return Err(CodecError::InputTooLarge {
                name,
                len: bytes.len() as u32,
            });
        }
        Ok(Self { input_seq, bytes })
    }
}

/// The outcome of a sequenced write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PtyInResult {
    /// Every byte reached the PTY.
    Ack { input_seq: u64, written: u32 },
    /// Nothing reached it, so a retry cannot duplicate.
    Reject {
        input_seq: u64,
        reason: PtyInRejectReason,
    },
    /// Some bytes reached it, so a retry WOULD duplicate. The caller must not
    /// resend; it must reconnect and resynchronise instead.
    Ambiguous {
        input_seq: u64,
        written: u32,
        reason: PtyInRejectReason,
    },
}

impl PtyInResult {
    /// Encode as `[input_seq:u64][written:u32][reason:u8]`. `Ack` carries a
    /// reason of zero, which is not a valid reject reason, so a decoder can
    /// never confuse the two.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(13);
        let (seq, written, reason) = match self {
            PtyInResult::Ack { input_seq, written } => (*input_seq, *written, 0),
            PtyInResult::Reject { input_seq, reason } => (*input_seq, 0, reason.code()),
            PtyInResult::Ambiguous {
                input_seq,
                written,
                reason,
            } => (*input_seq, *written, reason.code()),
        };
        write_sequence(&mut out, seq);
        out.extend_from_slice(&written.to_be_bytes());
        out.push(reason);
        out
    }

    /// Decode whichever of the three results the tag names.
    pub fn decode(frame_type: crate::codec::MuxFrameType, payload: &[u8]) -> Option<Self> {
        use crate::codec::MuxFrameType as T;
        let input_seq = read_sequence(payload, 0)?;
        let written = read_u32(payload, 8).unwrap_or(0);
        let reason = PtyInRejectReason::from_code(payload.get(12).copied().unwrap_or(0));
        Some(match frame_type {
            T::PtyInAck => PtyInResult::Ack { input_seq, written },
            T::PtyInReject => PtyInResult::Reject { input_seq, reason },
            T::PtyInAmbiguous => PtyInResult::Ambiguous {
                input_seq,
                written,
                reason,
            },
            _ => return None,
        })
    }
}

/// Why a write did not complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PtyInRejectReason {
    /// The channel does not exist.
    NoSuchChannel,
    /// The PTY has no reader.
    NoReader,
    /// The input queue is full; retry after draining.
    QueueFull,
    /// The child has exited.
    ChildExited,
    /// The write failed partway.
    PartialWrite,
}

impl PtyInRejectReason {
    pub fn code(self) -> u8 {
        match self {
            PtyInRejectReason::NoSuchChannel => 1,
            PtyInRejectReason::NoReader => 2,
            PtyInRejectReason::QueueFull => 3,
            PtyInRejectReason::ChildExited => 4,
            PtyInRejectReason::PartialWrite => 5,
        }
    }

    pub fn from_code(code: u8) -> Self {
        match code {
            2 => PtyInRejectReason::NoReader,
            3 => PtyInRejectReason::QueueFull,
            4 => PtyInRejectReason::ChildExited,
            5 => PtyInRejectReason::PartialWrite,
            // 0 is the Ack sentinel and anything higher is from a newer keeper.
            // Both mean the same to a client: treat it as no-such-channel rather
            // than guessing at a reason that would drive the wrong recovery.
            _ => PtyInRejectReason::NoSuchChannel,
        }
    }
}

/// A sequenced geometry change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResizeRequest {
    pub seq: u64,
    pub cols: u16,
    pub rows: u16,
}

impl ResizeRequest {
    /// Encode as `[seq:u64][cols:u32][rows:u32]`.
    pub fn encode(&self) -> Result<Vec<u8>, CodecError> {
        let cols = check_dimension(self.cols as u32)?;
        let rows = check_dimension(self.rows as u32)?;
        let mut out = Vec::with_capacity(16);
        write_sequence(&mut out, self.seq);
        out.extend_from_slice(&cols.to_be_bytes());
        out.extend_from_slice(&rows.to_be_bytes());
        Ok(out)
    }

    pub fn decode(payload: &[u8], name: &'static str) -> Result<Self, CodecError> {
        let Some(seq) = read_sequence(payload, 0) else {
            return Err(CodecError::TruncatedPayload { name, offset: 0 });
        };
        let Some(cols) = read_u32(payload, 8) else {
            return Err(CodecError::TruncatedPayload { name, offset: 8 });
        };
        let Some(rows) = read_u32(payload, 12) else {
            return Err(CodecError::TruncatedPayload { name, offset: 12 });
        };
        Ok(Self {
            seq,
            cols: check_dimension(cols)? as u16,
            rows: check_dimension(rows)? as u16,
        })
    }
}

/// The geometry the keeper actually applied, which is the answer to
/// `GetTerminalState` when a `ResizeAck` was lost and no retained marker can
/// still prove which sequence was consumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalState {
    pub applied_seq: u64,
    pub cols: u16,
    pub rows: u16,
}

impl TerminalState {
    /// Encode as `[seq:u64][cols:u32][rows:u32]`, the same shape as a
    /// `ResizeAck` so one decoder serves both.
    pub fn encode(&self) -> Result<Vec<u8>, CodecError> {
        ResizeRequest {
            seq: self.applied_seq,
            cols: self.cols,
            rows: self.rows,
        }
        .encode()
    }

    pub fn decode(payload: &[u8]) -> Result<Self, CodecError> {
        let ResizeRequest { seq, cols, rows } = ResizeRequest::decode(payload, "terminal state")?;
        Ok(Self {
            applied_seq: seq,
            cols,
            rows,
        })
    }
}

/// Wrap a sequenced-input payload in a frame.
pub fn pty_in_request_frame(
    channel_id: u16,
    request: &PtyInRequest,
) -> Result<MuxFrame, CodecError> {
    MuxFrame::new(
        crate::codec::MuxFrameType::PtyInRequest,
        channel_id,
        request.encode(),
    )
}
