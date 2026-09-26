//! The append, replay and capture-lane POLICY for a session's retained PTY
//! bytes. `session::emit` feeds the core, the keeper's reader thread calls
//! [`append_pty_chunk`], `session::lifecycle` calls [`replay_retained_into`]
//! when it adopts a survivor, and `browser_commands::scrollback_page` reads
//! what this leaves behind. Depends on `super::stream_scan`, `super::types` and
//! `roost_term` — and on nothing that depends on it back.
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
//! THE CORE WRITE IS NOT HERE. Feeding bytes to the emulator and answering the
//! capability probes they carry is `session::emit`'s, and the replies go back
//! through the keeper. This file hands over the retained window and the
//! advanced carries and nothing else, so a frozen core is never written by
//! accident.

use roost_term::TerminalCore;

use super::history::{UNHANDLED_SEQ_MAX, UnhandledSequenceEntry, UnhandledSequenceLog};
use super::stream_scan::{self, MODE_CARRY_MAX};
use super::types::SessionRecord;

/// Retain one chunk of PTY output and advance every piece of stream state the
/// record carries. Returns the offset of the chunk's END, so a caller can stamp
/// its upstream frame without a second read of the record.
///
/// `on_cwd_change` is invoked once per chunk with the folder an OSC 7 report
/// named. The emission itself belongs to the caller: a record holds no sink, and
/// a `cwd` event that reached the coordinator from here would cross a
/// durability boundary this file does not own.
pub fn append_pty_chunk(
    record: &mut SessionRecord,
    chunk: &[u8],
    on_cwd_change: &mut dyn FnMut(&str),
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
    on_cwd_change: &mut dyn FnMut(&str),
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
    if let Some(cwd) = osc7.cwd {
        if cwd != record.identity.cwd {
            tracing::debug!(
                session_id = %record.identity.session_id,
                channel_id = ?record.identity.channel_id,
                from = %record.identity.cwd,
                to = %cwd,
                "a session changed its working folder"
            );
            record.identity.cwd = cwd.clone();
            on_cwd_change(&cwd);
        }
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

/// What one unhandled-sequence sample did with what the core reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UnhandledRecord {
    /// Distinct sequences added to the log by this sample.
    pub recorded: u32,
    /// Sequences the core had already logged, reported again and dropped.
    pub repeated: u32,
    /// Whether the log is now at [`UNHANDLED_SEQ_MAX`] and refusing more.
    pub capped: bool,
}

/// Fold the sequences a core instance reported into the record's log.
///
/// `core_consumed_total` is the CORE's own cumulative count, which is what makes
/// the sample a watermark rather than a scan: nothing at or below the previous
/// reading is ever reported twice, however long the core keeps its ring.
/// `ring_dropped` is how many the core's own ring overwrote between two samples
/// — only their existence is knowable, and losing them silently would make the
/// log claim a completeness it does not have.
///
/// A sample that observed nothing and moved no watermark leaves the log `None`,
/// because a core that has never reported anything must cost nothing.
pub fn record_unhandled(
    log: &mut Option<UnhandledSequenceLog>,
    core_consumed_total: u64,
    observed: impl IntoIterator<Item = UnhandledSequenceEntry>,
    ring_dropped: u32,
    mono_ms: u64,
) -> UnhandledRecord {
    let mut observed = observed.into_iter().peekable();
    let previous = log.as_ref().map_or(0, |existing| existing.consumed);
    if core_consumed_total <= previous && ring_dropped == 0 && observed.peek().is_none() {
        return UnhandledRecord::default();
    }
    let mut record = log.take().unwrap_or_default();
    let mut summary = UnhandledRecord::default();
    for entry in observed {
        let key = unhandled_key(&entry);
        if record.keys.iter().any(|known| known == &key) {
            summary.repeated += 1;
            continue;
        }
        if record.entries.len() >= UNHANDLED_SEQ_MAX {
            record.capped = true;
            summary.repeated += 1;
            continue;
        }
        record.keys.push(key);
        record.entries.push(entry);
        summary.recorded += 1;
    }
    record.ring_dropped = record.ring_dropped.saturating_add(ring_dropped);
    record.consumed = core_consumed_total;
    summary.capped = record.capped;
    if record.consumed > 0 || !record.entries.is_empty() {
        *log = Some(record);
    }
    summary
}

/// What makes two reports of a sequence the same sequence: the code, its
/// private-parameter prefix, and its parameters. A repeat of a different
/// sequence with the same final byte is a different sequence, and collapsing
/// them would hide a novel one behind a familiar name.
fn unhandled_key(entry: &UnhandledSequenceEntry) -> String {
    format!(
        "{}\u{1}{}\u{1}{}\u{1}{:?}",
        entry.final_byte, entry.private, entry.param_count, entry.params
    )
}
