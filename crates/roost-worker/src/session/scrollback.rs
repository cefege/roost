//! The append, replay and capture-lane POLICY for a session's retained PTY
//! bytes, and the ordered query-reply lane's session half. `session::emit`
//! calls [`answer_terminal_queries`] (live) or [`advance_captured_query_carry`]
//! (capture) for every chunk; [`append_pty_chunk`] retains a chunk and advances
//! its stream state, [`replay_retained_into`] rebuilds a replacement core from
//! the ring, and `browser_commands::scrollback_page` reads what this leaves
//! behind. Ports the lane half of
//! `apps/worker/src/session/session-scrollback.ts`. Depends on
//! `super::stream_scan`, `super::query_reply`, `super::types` and `roost_term`.
//!
//! THE CONTAINER IS NOT HERE. `super::ring::ScrollbackRing` is a fixed-capacity
//! ring and `SessionRecord::append_retained` is the only writer of `head_seq`.
//! This file decides WHICH bytes are retained and what state a chunk advances;
//! it never opens the record's floor, and a call site that reached for
//! `record.scrollback.append` directly would break `floor + retained ==
//! head_seq` — the aliasing bug that re-aliases every absolute row index a
//! browser already holds.
//!
//! ONE APPEND, TWO LANES. The live path and the capture lane retain the same
//! bytes and advance the same carries; the only difference is that a
//! geometry-uncertain transaction FREEZES the core, so the capture lane's chunks
//! are never parsed by it. Stream state rather than grid state is what the scans
//! below maintain, which is exactly why they stay truthful while the core is
//! still — and why a partial capability probe left by a captured chunk cannot be
//! glued onto a post-rebuild chunk that never followed it.
//!
//! THE LIVE CORE WRITE IS [`answer_terminal_queries`]. It feeds the emulator in
//! probe-cut segments and hands the replies to the [`QueryReplyLane`], which
//! writes them back into the PTY in stream order. Every other core write is a
//! replay through `TerminalCore::write`, which discards what the bytes provoke:
//! a probe replayed from history is not a question anyone is waiting on.

use roost_protocol::wire::brand::SessionId;
use roost_term::TerminalCore;

use super::query_reply::{QUERY_CARRY_MAX, QueryReply, QueryReplyLane, answer_queries};
use super::stream_scan::{self, MODE_CARRY_MAX};
use super::types::SessionRecord;

/// Retain one chunk of PTY output and advance every piece of stream state the
/// record carries. Returns the offset of the chunk's END, so a caller can stamp
/// its upstream frame without a second read of the record.
///
/// `on_cwd_change` is invoked at most once per chunk, with the session and the
/// NEW folder an OSC 7 report named. The emission itself belongs to the caller
/// (`session::cwd_events`): a record holds no sink.
pub fn append_pty_chunk(
    record: &mut SessionRecord,
    chunk: &[u8],
    on_cwd_change: &mut dyn FnMut(&SessionId, &str),
) -> u64 {
    let head_seq = record.append_retained(chunk);
    scan_stream_state(record, chunk, on_cwd_change);
    tracing::trace!(
        session_id = %record.identity.session_id,
        channel_id = ?record.identity.channel_id,
        len = chunk.len(),
        head_seq,
        "a pty chunk was retained and its stream state advanced"
    );
    head_seq
}

/// Alt-screen mode, agent OSC evidence and OSC 7 cwd, in one pass over the
/// carried prefix plus this chunk.
///
/// The three are scanned together because they share the boundary question: a
/// sequence split across two chunks is only recognisable if the tail that
/// preceded the split is still in hand.
fn scan_stream_state(
    record: &mut SessionRecord,
    chunk: &[u8],
    on_cwd_change: &mut dyn FnMut(&SessionId, &str),
) {
    let mut mode_input = Vec::with_capacity(record.mode_carry.len() + chunk.len());
    mode_input.extend_from_slice(&record.mode_carry);
    mode_input.extend_from_slice(chunk);
    record.alt_mode = stream_scan::scan_alt_mode(&mode_input, record.alt_mode);
    record.mode_carry = tail(&mode_input, MODE_CARRY_MAX);

    let mut agent_input = Vec::with_capacity(record.agent_osc.carry.len() + chunk.len());
    agent_input.extend_from_slice(&record.agent_osc.carry);
    agent_input.extend_from_slice(chunk);
    let agent = stream_scan::scan_agent_osc(&agent_input);
    record.agent_osc.carry = agent.carry;
    // Only a title this chunk actually carried replaces the retained one: a
    // chunk with no OSC must not erase what a prompt set a moment ago.
    if let Some(title) = agent.title {
        record.agent_osc.raw_title = title;
    }
    if let Some(progress) = agent.progress {
        record.agent_osc.raw_progress = progress;
    }

    let mut cwd_input = Vec::with_capacity(record.osc7_carry.len() + chunk.len());
    cwd_input.extend_from_slice(&record.osc7_carry);
    cwd_input.extend_from_slice(chunk);
    let osc7 = stream_scan::scan_osc7(&cwd_input);
    record.osc7_carry = osc7.carry;
    if let Some(cwd) = osc7.cwd
        && cwd != record.identity.cwd
    {
        tracing::debug!(
            session_id = %record.identity.session_id,
            channel_id = ?record.identity.channel_id,
            from = %record.identity.cwd,
            to = %cwd,
            "a session changed its working folder"
        );
        on_cwd_change(&record.identity.session_id, &cwd);
        record.identity.cwd = cwd;
    }
}

/// The last `keep` bytes of a combined buffer, or all of it when it is shorter.
fn tail(combined: &[u8], keep: usize) -> Vec<u8> {
    if combined.len() <= keep {
        return combined.to_vec();
    }
    combined[combined.len() - keep..].to_vec()
}

/// What a bounded replay fed a replacement core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Replay {
    /// Bytes handed to the core, in the order the ring holds them.
    pub bytes: u64,
    /// The record's head offset after the replay. A core that has consumed the
    /// whole ring is addressable from here, which is what makes the browser's
    /// absolute row indexes survive a rebuild.
    pub head_seq: u64,
    /// The window was at capacity, so the core's history is as deep as the ring
    /// allows and a row the old core held may not exist in the new one. This is
    /// what a rebuild's `ring_evicted` pin records, and it is why the loss is
    /// resize-induced rather than ordinary eviction.
    pub evicted: bool,
}

/// Feed a replacement core the whole retained window, oldest first.
///
/// `core` is a FRESH emulator: it has parsed nothing, so ordering is the whole
/// contract — the bytes go in exactly as the ring holds them. The copy is the
/// core's own `write(&[u8])` boundary rather than an avoidable allocation; a
/// streaming write would hand the emulator a slice of a window it is about to
/// be resized out of.
pub fn replay_retained_into(core: &mut dyn TerminalCore, record: &SessionRecord) -> Replay {
    let bytes = record.scrollback.to_vec();
    let length = bytes.len() as u64;
    core.write(&bytes);
    let replay = Replay {
        bytes: length,
        head_seq: record.head_seq,
        evicted: record.scrollback.evicting(),
    };
    tracing::info!(
        session_id = %record.identity.session_id,
        channel_id = ?record.identity.channel_id,
        bytes = replay.bytes,
        head_seq = replay.head_seq,
        evicted = replay.evicted,
        "retained pty bytes were replayed into a replacement core"
    );
    replay
}

/// Feed a LIVE chunk to the core and queue what its probes are owed for the
/// PTY. The chunk's bytes reach the core exactly once, here, in probe-cut
/// segments; natives and synthesized replies leave as one batch in probe
/// order, and a chunk that held no probe and left nothing queued sends nothing.
pub fn answer_terminal_queries(record: &mut SessionRecord, chunk: &[u8], replies: &QueryReplyLane) {
    let reply = answer_queries(
        &mut record.query_carry,
        Some(record.terminal_core.as_mut()),
        chunk,
    );
    note_query_reply(record, &reply);
    if reply.bytes.is_empty() {
        return;
    }
    tracing::debug!(
        session_id = %record.identity.session_id,
        channel_id = %record.identity.channel_id,
        native_len = reply.native_bytes,
        synth_len = reply.synth_bytes,
        "capability probes were answered and the replies queued for the pty"
    );
    replies.send(&record.identity.session_id, reply.bytes.into_bytes());
}

/// Advance the tokenizer over a chunk the capture lane retained but the
/// frozen core never parses. The stream moved, so the carry moves with it: a
/// partial probe left here must not be glued onto a chunk that never followed
/// it. Its own probes are answered by the post-boundary replay, not here.
pub fn advance_captured_query_carry(record: &mut SessionRecord, chunk: &[u8]) {
    let reply = answer_queries(&mut record.query_carry, None, chunk);
    note_query_reply(record, &reply);
}

/// Say what a chunk's answer withheld: an abandoned partial probe, and native
/// replies v2's core never sent. Both are state an application believes it
/// negotiated, so neither may vanish unexplained.
fn note_query_reply(record: &SessionRecord, reply: &QueryReply) {
    if reply.dropped_carry > 0 {
        tracing::debug!(
            session_id = %record.identity.session_id,
            channel_id = %record.identity.channel_id,
            bytes = reply.dropped_carry,
            cap = QUERY_CARRY_MAX,
            "an unterminated control sequence outgrew the probe carry and was abandoned"
        );
    }
    if reply.withheld_native > 0 {
        tracing::debug!(
            session_id = %record.identity.session_id,
            channel_id = %record.identity.channel_id,
            count = reply.withheld_native,
            "the core answered probes v2's core left unanswered; the replies were withheld"
        );
    }
}
