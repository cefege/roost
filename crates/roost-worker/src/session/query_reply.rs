//! The capability-probe tokenizer and the single ordered reply lane's core
//! half: feed one PTY chunk to the core and say what the application is owed
//! for it, in probe order. `session::scrollback` calls [`answer_queries`] for
//! every chunk; nothing else writes a live core with `write_raw`. Ports
//! `apps/worker/src/terminal/terminal-query-reply.ts`.
//!
//! TWO SOURCES FEED ONE LANE, and the application reads both off one stdin:
//!   native     probes the core answers itself — the cursor report (`CSI 6n`).
//!              `get_response` pops ONE queued reply, so the lane drains until
//!              `None` after every segment it writes.
//!   synthetic  probes the core must stay silent on and Roost answers: Primary
//!              DA (`CSI c`, `CSI 0c`) and XTVERSION (`CSI > q`, `CSI > 0 q`).
//! Ordering falls out of SEGMENTING the write: the chunk is fed to the core in
//! pieces cut at each synthetic probe's end, the natives those bytes produced
//! are drained first, and only then is the synthesized reply appended.
//! Concatenating every native ahead of every synthesized reply would answer
//! `CSI c` then `CSI 6n` backwards.
//!
//! v2's core answered cursor and Kitty keyboard queries (v2 muted Kitty).
//! Alacritty also answers DA1 (`?6c`), DA2, `CSI 5n`, DECRQM and `CSI 18t`;
//! those are WITHHELD so DA1 is synthesized once. Cursor and Kitty reports pass.
//!
//! A Kitty keyboard query is segmented like a synthesized probe to preserve ordering; replay uses [`TerminalCore::write`].
//!
//! THE WRITE-BACK. [`QueryReplyLane`] is the synchronous sending half the
//! ingest path holds; [`QueryReplyWriter::run`] writes each batch through
//! `SessionManager::write_worker_owned_input`, ONE batch in flight per session,
//! because the keeper's ordering slot is taken when a write is first polled
//! and two unawaited writes can swap.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use futures_util::StreamExt as _;
use futures_util::stream::FuturesUnordered;
use roost_protocol::wire::brand::SessionId;
use roost_term::TerminalCore;
use tokio::sync::mpsc;

use super::input_write::WorkerInputResult;
use super::lifecycle::SessionManager;
use crate::uplink::OwnerFuture;

const ESC: u8 = 0x1b;
const CSI_OPEN: u8 = b'[';
// CSI body: parameter bytes 0x30-0x3f and intermediate bytes 0x20-0x2f. Their
// order is not enforced — classification needs the span and the final byte,
// and a malformed order is not a probe either way.
const BODY_MIN: u8 = 0x20;
const BODY_MAX: u8 = 0x3f;
const FINAL_MIN: u8 = 0x40;
const FINAL_MAX: u8 = 0x7e;
/// A private marker (`<` `=` `>` `?`) occupies the first parameter position.
const PRIVATE_MIN: u8 = 0x3c;
const PRIVATE_MAX: u8 = 0x3f;

/// Primary DA reply: VT100 with Advanced Video Option — the universal "I am a
/// terminal" handshake, enough to unblock any DA-gated init.
pub const PRIMARY_DA_REPLY: &str = "\x1b[?1;2c";
/// XTVERSION reply, `DCS > | name ST`: a client that version-gates behaviour
/// sees a name instead of silence. v2's bytes, verbatim.
pub const XTVERSION_REPLY: &str = "\x1bP>|wterm(roost)\x1b\\";

/// The longest probe answered is 5 bytes and the longest CSI tokenized past
/// (DECRQM, `ESC [ ? 2 0 2 6 $ p`) is 9, so no legitimate partial needs more.
/// Past the cap an unterminated CSI is abandoned: a stream that opens a CSI and
/// never closes it cannot pin worker memory, and the discarded bytes hold no
/// further ESC to re-anchor on.
pub const QUERY_CARRY_MAX: usize = 32;

/// What one chunk owes the application.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueryReply {
    /// Natives and synthesized replies interleaved in probe order: the one
    /// batch written back to the PTY.
    pub bytes: String,
    /// How many of `bytes` the core produced.
    pub native_bytes: usize,
    /// How many of `bytes` Roost synthesized.
    pub synth_bytes: usize,
    /// Bytes of an unterminated CSI abandoned at [`QUERY_CARRY_MAX`].
    pub dropped_carry: usize,
    /// Native replies v2's core never sent, withheld rather than forwarded.
    pub withheld_native: u32,
}

/// Feed `chunk` to `core` and return the replies it is owed, in probe order.
///
/// `carry` is the session's tokenizer carry (`SessionRecord::query_carry`):
/// EVERY byte of the PTY stream advances it, so a probe split across chunks
/// at any offset is recognised exactly once. `None` for `core` is the capture
/// lane: a geometry-uncertain transaction froze the core, so nothing is parsed
/// or answered, but the carry still moves and only `dropped_carry` means
/// anything.
pub fn answer_queries(
    carry: &mut Vec<u8>,
    mut core: Option<&mut dyn TerminalCore>,
    chunk: &[u8],
) -> QueryReply {
    let shift = carry.len();
    let joined;
    // Zero-copy on the overwhelmingly common carry-free chunk.
    let buf: &[u8] = if shift == 0 {
        chunk
    } else {
        joined = [carry.as_slice(), chunk].concat();
        &joined
    };
    let mut reply = QueryReply::default();
    // Chunk bytes already handed to the core.
    let mut cursor = 0;
    let mut carry_from = buf.len();
    let mut at = 0;
    while at < buf.len() {
        let Some(esc) = buf[at..]
            .iter()
            .position(|&byte| byte == ESC)
            .map(|found| found + at)
        else {
            break;
        };
        if esc + 1 == buf.len() {
            carry_from = esc;
            break;
        }
        if buf[esc + 1] != CSI_OPEN {
            at = esc + 1;
            continue;
        }
        let mut end = esc + 2;
        while end < buf.len() && (BODY_MIN..=BODY_MAX).contains(&buf[end]) {
            end += 1;
        }
        if end == buf.len() {
            if buf.len() - esc > QUERY_CARRY_MAX {
                reply.dropped_carry = buf.len() - esc;
            } else {
                carry_from = esc;
            }
            break;
        }
        let final_byte = buf[end];
        // A byte outside the final range (C0, or the ESC of the next sequence)
        // aborts this CSI; rescan from just after its ESC.
        if !(FINAL_MIN..=FINAL_MAX).contains(&final_byte) {
            at = esc + 1;
            continue;
        }
        at = end + 1;
        let Some(synthesized) = synthesized_reply(&buf[esc + 2..end], final_byte) else {
            continue;
        };
        let Some(core) = core.as_deref_mut() else {
            continue;
        };
        // The carry only ever holds an UNTERMINATED CSI, so a complete probe's
        // final byte is always inside this chunk: `at - shift` is a real offset.
        let probe_end = at - shift;
        core.write_raw(&chunk[cursor..probe_end]);
        cursor = probe_end;
        drain_core_replies(core, &mut reply);
        reply.bytes.push_str(synthesized);
        reply.synth_bytes += synthesized.len();
    }
    if let Some(core) = core {
        if cursor < chunk.len() {
            core.write_raw(&chunk[cursor..]);
        }
        drain_core_replies(core, &mut reply);
    }
    // Always a copy: `buf` may borrow the reader's reusable buffer.
    *carry = if carry_from < buf.len() {
        buf[carry_from..].to_vec()
    } else {
        Vec::new()
    };
    reply
}

/// Pop every queued core reply, oldest first, forwarding only the ones v2's
/// core also sent.
fn drain_core_replies(core: &mut dyn TerminalCore, reply: &mut QueryReply) {
    while let Some(native) = core.get_response() {
        if native.is_empty() {
            continue;
        }
        if !is_cursor_position_report(&native) && !is_kitty_keyboard_report(&native) {
            reply.withheld_native += 1;
            continue;
        }
        reply.native_bytes += native.len();
        reply.bytes.push_str(&native);
    }
}

/// `ESC [ <row> ; <col> R`: the one native reply v2's core produced that the
/// lane forwards.
fn is_cursor_position_report(reply: &str) -> bool {
    let Some(body) = reply
        .strip_prefix("\x1b[")
        .and_then(|rest| rest.strip_suffix('R'))
    else {
        return false;
    };
    let Some((row, col)) = body.split_once(';') else {
        return false;
    };
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    digits(row) && digits(col)
}

/// `CSI ? flags u`: a query reply from the live terminal core.
fn is_kitty_keyboard_report(reply: &str) -> bool {
    let Some(flags) = reply
        .strip_prefix("\x1b[?")
        .and_then(|rest| rest.strip_suffix('u'))
    else {
        return false;
    };
    flags.bytes().all(|byte| byte.is_ascii_digit())
}

/// A synthesized reply for one complete CSI, or `Some("")` to drain a native
/// reply at that probe boundary; `None` means the CSI is not a handled probe.
fn synthesized_reply(body: &[u8], final_byte: u8) -> Option<&'static str> {
    let private = match body.first() {
        Some(&first) if (PRIVATE_MIN..=PRIVATE_MAX).contains(&first) => first,
        _ => 0,
    };
    let params = if private == 0 { body } else { &body[1..] };
    match (final_byte, private) {
        (b'c', 0) if zero_params(params) => Some(PRIMARY_DA_REPLY),
        (b'q', b'>') if zero_params(params) => Some(XTVERSION_REPLY),
        (b'u', b'?') if params.is_empty() => Some(""),
        _ => None,
    }
}

/// Primary DA and XTVERSION both take Ps=0, defaulting to 0 when omitted; any
/// other parameter makes it a different request (`CSI > c` is DA2, `CSI ? … $ p`
/// is DECRQM), tokenized here so it can never split the stream wrongly, and
/// never answered here.
fn zero_params(params: &[u8]) -> bool {
    params.iter().all(|&byte| byte == b'0' || byte == b';')
}

/// Where a reply batch is written: worker-originated PTY input with no
/// coordinator sequence. [`SessionManager`] is the production implementation.
pub trait WorkerOwnedInput: Send + Sync + 'static {
    fn write_worker_owned_input(
        &self,
        session_id: &SessionId,
        bytes: Vec<u8>,
    ) -> OwnerFuture<WorkerInputResult>;
}

impl WorkerOwnedInput for SessionManager {
    fn write_worker_owned_input(
        &self,
        session_id: &SessionId,
        bytes: Vec<u8>,
    ) -> OwnerFuture<WorkerInputResult> {
        SessionManager::write_worker_owned_input(self, session_id, bytes)
    }
}

/// One chunk's replies, addressed to the session whose application asked.
#[derive(Debug)]
struct ReplyBatch {
    session_id: SessionId,
    bytes: Vec<u8>,
}

/// The sending half of the reply lane: synchronous and non-blocking, so the
/// keeper's dispatch thread can hand a batch over mid-ingest.
#[derive(Debug, Clone, Default)]
pub struct QueryReplyLane {
    /// `None` only for [`QueryReplyLane::detached`].
    sender: Option<mpsc::UnboundedSender<ReplyBatch>>,
}

impl QueryReplyLane {
    /// A lane and the writer that must be run for anything sent on it to reach
    /// a PTY.
    pub fn new() -> (Self, QueryReplyWriter) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (
            Self {
                sender: Some(sender),
            },
            QueryReplyWriter { receiver },
        )
    }

    /// A lane with no writer: every batch is dropped, and each drop is logged.
    pub fn detached() -> Self {
        Self { sender: None }
    }

    /// Queue one batch behind every earlier batch for the same session.
    /// `false` when no writer will ever write it.
    pub fn send(&self, session_id: &SessionId, bytes: Vec<u8>) -> bool {
        let len = bytes.len();
        let queued = self.sender.as_ref().is_some_and(|sender| {
            sender
                .send(ReplyBatch {
                    session_id: session_id.clone(),
                    bytes,
                })
                .is_ok()
        });
        if !queued {
            tracing::warn!(
                %session_id,
                len,
                "a query reply was dropped: no reply writer is running"
            );
        }
        queued
    }
}

/// The receiving half: writes every batch to its session's PTY.
#[derive(Debug)]
pub struct QueryReplyWriter {
    receiver: mpsc::UnboundedReceiver<ReplyBatch>,
}

impl QueryReplyWriter {
    /// Write batches until every [`QueryReplyLane`] is dropped and the backlog
    /// is written. One write per session is in flight at a time and the next
    /// starts only once it resolves; sessions never wait on one another.
    pub async fn run<W: WorkerOwnedInput>(mut self, input: Arc<W>) {
        let mut backlog: HashMap<SessionId, VecDeque<Vec<u8>>> = HashMap::new();
        let mut writing = FuturesUnordered::new();
        let mut open = true;
        tracing::info!("the query-reply writer started");
        while open || !writing.is_empty() {
            tokio::select! {
                received = self.receiver.recv(), if open => match received {
                    Some(batch) => match backlog.get_mut(&batch.session_id) {
                        Some(waiting) => waiting.push_back(batch.bytes),
                        None => {
                            backlog.insert(batch.session_id.clone(), VecDeque::new());
                            writing.push(write_batch(&input, batch.session_id, batch.bytes));
                        }
                    },
                    None => open = false,
                },
                Some(session_id) = writing.next(), if !writing.is_empty() => {
                    match backlog.get_mut(&session_id).and_then(VecDeque::pop_front) {
                        Some(bytes) => writing.push(write_batch(&input, session_id, bytes)),
                        None => {
                            backlog.remove(&session_id);
                        }
                    }
                }
            }
        }
        tracing::info!("the query-reply writer stopped: every lane was dropped");
    }
}

/// One batch's write, resolving to the session it was for once the keeper has
/// answered, so the writer knows whose next batch may start.
async fn write_batch<W: WorkerOwnedInput>(
    input: &Arc<W>,
    session_id: SessionId,
    bytes: Vec<u8>,
) -> SessionId {
    let len = bytes.len();
    let write = input.write_worker_owned_input(&session_id, bytes);
    match write.await {
        WorkerInputResult::Accepted { written_bytes } => tracing::debug!(
            %session_id,
            len,
            written_bytes,
            "query replies reached the pty"
        ),
        WorkerInputResult::Rejected { reason } => tracing::warn!(
            %session_id,
            len,
            %reason,
            "query replies were refused before the pty write"
        ),
        WorkerInputResult::Ambiguous {
            written_bytes,
            reason,
        } => tracing::warn!(
            %session_id,
            len,
            written_bytes,
            %reason,
            "query replies may have reached the pty only in part"
        ),
    }
    session_id
}
