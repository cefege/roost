//! The framed keeper wire codec: the envelope, the tag table, and the scalar
//! codecs the binary payloads share. Owned by the keeper and by the worker's
//! client; nothing else encodes or decodes these bytes.
//!
//! The contract is `protocol/spec/keeper.md` and it is authoritative. A tag
//! never changes meaning, and a tag that does is a change to that document, not
//! to this file alone.

use serde::{Deserialize, Serialize};

/// A frame's total length, which covers every byte after the length field
/// itself. A decoder rejects a larger value rather than allocating for it: a
/// length that large is a protocol violation, not a frame.
pub const KEEPER_MAX_MUX_FRAME_BYTES: u32 = 16 * 1024 * 1024;
/// The smallest legal frame body: a type tag and a two-byte channel id, with no
/// payload at all. A claimed length below this cannot be indexed, so the
/// decoder refuses it rather than trusting the prefix to be well formed.
pub const MUX_FRAME_HEADER_BYTES: u32 = 3;
/// A single `PtyInRequest` payload. Bounded so one keystroke storm cannot make
/// the keeper allocate without limit.
pub const KEEPER_MAX_INPUT_BYTES: u32 = 64 * 1024;
/// A PTY's largest row or column count.
pub const KEEPER_MAX_TERMINAL_DIMENSION: u32 = 0xffff;
/// Resize records retained per channel for ordered history replay.
pub const KEEPER_MAX_HISTORY_RESIZE_RECORDS: u32 = 4096;

/// Frame type tags. The discriminants are the wire values and must never be
/// reused for a different meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MuxFrameType {
    // -- channel lifecycle -------------------------------------------------
    /// client → keeper: open a new PTY.
    Spawn = 0x10,
    /// keeper → client: the PTY was allocated.
    SpawnAck = 0x11,
    /// keeper → client: the PTY could not be allocated.
    SpawnErr = 0x12,
    /// client → keeper: terminate the PTY child.
    KillChild = 0x31,
    /// keeper → client: the PTY child exited.
    Exit = 0x32,

    // -- input -------------------------------------------------------------
    /// client → keeper: raw input, unacknowledged (legacy).
    PtyIn = 0x20,
    /// keeper → client: raw PTY output.
    PtyOut = 0x21,
    /// client → keeper: sequenced input.
    PtyInRequest = 0x22,
    /// keeper → client: the sequence was written in full.
    PtyInAck = 0x23,
    /// keeper → client: nothing was written.
    PtyInReject = 0x24,
    /// keeper → client: a partial write, so a retry would duplicate.
    PtyInAmbiguous = 0x25,

    // -- geometry ----------------------------------------------------------
    /// client → keeper: JSON geometry, unacknowledged (legacy).
    Resize = 0x30,
    /// client → keeper: sequenced geometry.
    ResizeRequest = 0x33,
    /// keeper → client: the sequence was applied.
    ResizeAck = 0x34,
    /// keeper → client: the geometry was refused.
    ResizeReject = 0x35,
    /// client → keeper: query the last applied sequence.
    ResizeStatus = 0x36,

    // -- cross-process resume ---------------------------------------------
    /// client → keeper: which channels are still live.
    ListChannels = 0xE0,
    /// keeper → client: the live channels.
    ListChannelsResp = 0xE1,
    /// client → keeper: capability-bearing hello.
    Hello = 0xE2,
    /// keeper → client: contract and observation.
    HelloResp = 0xE3,
    /// client → keeper: drain history (legacy).
    GetHistory = 0xE4,
    /// keeper → client: the head sequence and the raw ring.
    GetHistoryResp = 0xE5,
    /// client → keeper: ordered history records.
    GetHistoryRecords = 0xE6,
    /// keeper → client: the ordered records.
    GetHistoryRecordsResp = 0xE7,

    // -- administration ----------------------------------------------------
    /// client → keeper: deliberate offline maintenance.
    Shutdown = 0xE8,
    /// keeper → client: the keeper is stopping.
    ShutdownAck = 0xE9,
    /// client → keeper: recover authoritative geometry.
    GetTerminalState = 0xEA,
    /// keeper → client: the live resize state.
    GetTerminalStateResp = 0xEB,
    /// client → keeper: stop only if no channel is live.
    ShutdownIfEmpty = 0xEC,
    /// keeper → client: the keeper was empty and is stopping.
    ShutdownIfEmptyAck = 0xED,
    /// keeper → client: a channel is live, so the keeper stays.
    ShutdownIfEmptyReject = 0xEE,

    // -- liveness ----------------------------------------------------------
    /// both: liveness probe.
    Ping = 0xF0,
    /// both: liveness response.
    Pong = 0xF1,
}

impl MuxFrameType {
    /// The wire value, which the enum guarantees by construction.
    pub fn tag(self) -> u8 {
        self as u8
    }

    /// Recover a tag from the wire, rejecting one this build does not know.
    ///
    /// An unknown tag is not a fatal protocol error: it is how a newer keeper
    /// says it has a frame this client predates. The caller decides whether to
    /// skip it, which it can only do safely once the length has been read.
    pub fn from_tag(tag: u8) -> Option<Self> {
        use MuxFrameType::*;
        Some(match tag {
            0x10 => Spawn,
            0x11 => SpawnAck,
            0x12 => SpawnErr,
            0x20 => PtyIn,
            0x21 => PtyOut,
            0x22 => PtyInRequest,
            0x23 => PtyInAck,
            0x24 => PtyInReject,
            0x25 => PtyInAmbiguous,
            0x30 => Resize,
            0x31 => KillChild,
            0x32 => Exit,
            0x33 => ResizeRequest,
            0x34 => ResizeAck,
            0x35 => ResizeReject,
            0x36 => ResizeStatus,
            0xE0 => ListChannels,
            0xE1 => ListChannelsResp,
            0xE2 => Hello,
            0xE3 => HelloResp,
            0xE4 => GetHistory,
            0xE5 => GetHistoryResp,
            0xE6 => GetHistoryRecords,
            0xE7 => GetHistoryRecordsResp,
            0xE8 => Shutdown,
            0xE9 => ShutdownAck,
            0xEA => GetTerminalState,
            0xEB => GetTerminalStateResp,
            0xEC => ShutdownIfEmpty,
            0xED => ShutdownIfEmptyAck,
            0xEE => ShutdownIfEmptyReject,
            0xF0 => Ping,
            0xF1 => Pong,
            _ => return None,
        })
    }
}

/// One framed message. `channel_id` 0 is the control lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MuxFrame {
    pub frame_type: MuxFrameType,
    pub channel_id: u16,
    pub payload: Vec<u8>,
}

/// Why a byte stream could not be turned into frames. Each variant is a
/// protocol violation that ends the connection, not a retryable condition.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodecError {
    #[error("frame length {0} exceeds the {KEEPER_MAX_MUX_FRAME_BYTES} byte maximum")]
    FrameTooLarge(u32),
    #[error("frame body is {actual} bytes but its length prefix claims {claimed}")]
    LengthMismatch { claimed: u32, actual: usize },
    #[error("a {name} payload is missing its {offset}-byte header")]
    TruncatedPayload { name: &'static str, offset: usize },
    #[error("a terminal dimension of {0} is outside 1..={KEEPER_MAX_TERMINAL_DIMENSION}")]
    BadDimension(u32),
    #[error("a {name} payload of {len} bytes exceeds the {KEEPER_MAX_INPUT_BYTES} byte maximum")]
    InputTooLarge { name: &'static str, len: u32 },
    #[error("payload is not valid JSON for {name}: {reason}")]
    BadJson { name: &'static str, reason: String },
}

/// Append a big-endian `u64`. Unaligned, like every scalar in the protocol.
pub fn write_sequence(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_be_bytes());
}

/// Read a big-endian `u64` at `offset`, or `None` if the buffer is short.
pub fn read_sequence(payload: &[u8], offset: usize) -> Option<u64> {
    let end = offset.checked_add(8)?;
    let bytes = payload.get(offset..end)?;
    Some(u64::from_be_bytes(bytes.try_into().ok()?))
}

/// Read a big-endian `u32` at `offset`, or `None` if the buffer is short.
pub fn read_u32(payload: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    let bytes = payload.get(offset..end)?;
    Some(u32::from_be_bytes(bytes.try_into().ok()?))
}

/// Why a dimension is unacceptable. Kept separate from `CodecError` because
/// the spec's rule is to reject rather than clamp, and a caller that clamps
/// anyway has a bug this makes visible.
pub fn check_dimension(value: u32) -> Result<u32, CodecError> {
    if value == 0 || value > KEEPER_MAX_TERMINAL_DIMENSION {
        return Err(CodecError::BadDimension(value));
    }
    Ok(value)
}

impl MuxFrame {
    /// Encode one frame, including its length prefix.
    pub fn encode(&self) -> Vec<u8> {
        let body = 1 + 2 + self.payload.len();
        let mut out = Vec::with_capacity(body + 4);
        out.extend_from_slice(&(body as u32).to_be_bytes());
        out.push(self.frame_type.tag());
        out.extend_from_slice(&self.channel_id.to_be_bytes());
        out.extend_from_slice(&self.payload);
        out
    }

    /// Build a frame, checking the payload bound that applies to this tag.
    pub fn new(
        frame_type: MuxFrameType,
        channel_id: u16,
        payload: Vec<u8>,
    ) -> Result<Self, CodecError> {
        if payload.len() as u64 > KEEPER_MAX_MUX_FRAME_BYTES as u64 {
            return Err(CodecError::FrameTooLarge(payload.len() as u32));
        }
        Ok(Self {
            frame_type,
            channel_id,
            payload,
        })
    }

    /// Build a frame from a JSON-serialisable payload.
    pub fn json<T: Serialize>(
        frame_type: MuxFrameType,
        channel_id: u16,
        value: &T,
    ) -> Result<Self, CodecError> {
        let payload = serde_json::to_vec(value).map_err(|err| CodecError::BadJson {
            name: "frame",
            reason: err.to_string(),
        })?;
        Self::new(frame_type, channel_id, payload)
    }

    /// Decode this frame's JSON payload, or `None` if it does not parse.
    pub fn parse_json<T: for<'de> Deserialize<'de>>(&self) -> Option<T> {
        serde_json::from_slice(&self.payload).ok()
    }
}

/// A frame pulled off a stream, or the bytes that could not be used yet.
#[derive(Debug)]
pub enum StreamEvent {
    /// One complete frame. An unknown tag is reported as its raw tag so the
    /// caller can skip it once it trusts the length.
    Frame {
        frame_type: Option<MuxFrameType>,
        raw_tag: u8,
        channel_id: u16,
        payload: Vec<u8>,
    },
    /// The stream violated the protocol and cannot continue.
    Failed(CodecError),
}

/// A streaming decoder. Feed it whatever a read returned; it returns the
/// frames that are now complete and keeps the remainder for the next read.
///
/// This exists because a socket read splits wherever it likes, and a decoder
/// that assumed one frame per read would corrupt every large PTY burst.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buffered: Vec<u8>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self {
            buffered: Vec::new(),
        }
    }

    /// Bytes held back because they do not yet form a whole frame.
    pub fn buffered_len(&self) -> usize {
        self.buffered.len()
    }

    /// Feed bytes from one read and take every frame they complete.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<StreamEvent> {
        self.buffered.extend_from_slice(bytes);
        let mut events = Vec::new();
        // A frame stays buffered until its whole body has arrived, so the
        // loop ends on an incomplete prefix rather than on a frame count.
        while let Some(prefix) = self.buffered.get(..4) {
            let claimed = u32::from_be_bytes(prefix.try_into().expect("sliced to 4 bytes"));
            if claimed > KEEPER_MAX_MUX_FRAME_BYTES {
                events.push(StreamEvent::Failed(CodecError::FrameTooLarge(claimed)));
                self.buffered.clear();
                return events;
            }
            // A frame body is a tag byte plus a two-byte channel id before any
            // payload, so a claimed length below that is a protocol violation
            // and not a frame that has not finished arriving.
            //
            // This bound is the whole reason the four bytes below cannot be
            // indexed unguarded: with only the upper check, a peer that sends
            // `00 00 00 00` gets an empty frame and `frame[0]` panics, and the
            // panic unwinds the serve loop and the keeper's main, taking every
            // PTY the machine held with it. Any same-uid process can write
            // those four bytes to the socket, so an unguarded index here is a
            // four-byte denial of service against every terminal on a host.
            //
            // v2 refused the same frame: `protocol-envelope.ts:114` throws a
            // `RangeError` when the body is under three bytes. The port kept
            // the indexing and lost the check, and `LengthMismatch` was
            // declared for exactly this and never constructed by the decoder.
            if claimed < MUX_FRAME_HEADER_BYTES {
                events.push(StreamEvent::Failed(CodecError::LengthMismatch {
                    claimed,
                    actual: self.buffered.len().saturating_sub(4),
                }));
                self.buffered.clear();
                return events;
            }
            let total = 4 + claimed as usize;
            if self.buffered.len() < total {
                break;
            }
            // Take the frame out rather than borrowing across the drain: the
            // payload is copied anyway, and holding a borrow here would pin
            // the buffer for the whole loop.
            let frame: Vec<u8> = self.buffered.drain(..total).skip(4).collect();

            let raw_tag = frame[0];
            let channel_id = u16::from_be_bytes([frame[1], frame[2]]);
            let payload = frame[3..].to_vec();
            events.push(StreamEvent::Frame {
                frame_type: MuxFrameType::from_tag(raw_tag),
                raw_tag,
                channel_id,
                payload,
            });
        }
        events
    }
}
