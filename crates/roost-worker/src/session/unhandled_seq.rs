//! Core-reported unhandled escape sequences, per session: sample the core's
//! never-cleared ring against the record's own high-water mark, and project the
//! result for the diagnostics snapshot. The emitter samples on every frame, the
//! diagnostic snapshot samples before it reads. Ports
//! `apps/worker/src/session/session-unhandled-seq.ts`.
//!
//! THE RING IS NEVER CLEARED, so reporting straight off its window would re-fire
//! the same stale entry on every frame; `UnhandledSequenceLog::consumed` is the
//! mark held against the ring's total. PARTIAL DETECTOR BY CONSTRUCTION: the
//! core logs CSI its dispatcher drops, not an OSC or a mode number it accepts
//! and ignores, so an empty list is not proof the core understood everything.

use roost_observability::{LogFields, SignalKind, signal};
use roost_term::UnhandledSequence;

use super::history::{UNHANDLED_SEQ_MAX, UnhandledSequenceEntry, UnhandledSequenceLog};
use super::types::SessionRecord;

/// One distinct sequence as the diagnostic snapshot reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnhandledSequenceSnapshotEntry {
    pub final_byte: String,
    pub private: String,
    pub param_count: u32,
    pub params: Vec<u32>,
    pub first_seen_mono_ms: u64,
}

/// What the diagnostic snapshot says about one session's core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnhandledSequenceSnapshot {
    /// Distinct sequences, oldest first, never more than [`UNHANDLED_SEQ_MAX`].
    pub entries: Vec<UnhandledSequenceSnapshotEntry>,
    /// Sequences this core has logged in total, duplicates included.
    pub logged_total: u64,
    /// Entries the core's ring overwrote before Roost read them.
    pub ring_dropped: u32,
    /// `entries` is full: later DISTINCT sequences were neither recorded nor
    /// counted, so this is the first [`UNHANDLED_SEQ_MAX`], not necessarily all.
    pub capped: bool,
}

/// Sample the core's ring and record only what this core has never reported.
///
/// Steady state is one integer compare against the mark, allocating nothing,
/// which is why this can run on every emitted frame. The window is decoded
/// only when the ring's total has moved.
pub fn note_unhandled_sequences(record: &mut SessionRecord, now_mono_ms: u64) {
    let ring = record.terminal_core.unhandled_sequences();
    let total = ring.total();
    if total == 0 {
        return;
    }
    let log = record
        .unhandled
        .get_or_insert_with(UnhandledSequenceLog::default);
    if total == log.consumed {
        return;
    }
    let mark = log.consumed;
    log.consumed = total;
    // At the cap the accumulator can learn nothing more, so it stops decoding
    // the window entirely; the mark keeps advancing for free.
    if log.capped {
        return;
    }
    // The window holds the newest `retained` logical entries: element i is
    // logical index `oldest + i`. Anything between the mark and `oldest` was
    // overwritten before Roost read it — counted, because a session that
    // outruns the ring between two frames is itself the finding.
    let retained = total.min(ring.capacity() as u64);
    let oldest = total - retained;
    if oldest > mark {
        let lost = u32::try_from(oldest - mark).unwrap_or(u32::MAX);
        log.ring_dropped = log.ring_dropped.saturating_add(lost);
    }
    let skip = mark.saturating_sub(oldest) as usize;
    for sequence in ring.window().skip(skip) {
        let entry = snapshot_entry(sequence, now_mono_ms);
        let key = sequence_key(&entry);
        if log.keys.iter().any(|known| known == &key) {
            continue;
        }
        log.keys.push(key);
        tracing::info!(
            session_id = %record.identity.session_id,
            channel_id = %record.identity.channel_id,
            final_byte = %entry.final_byte,
            private = %entry.private,
            param_count = entry.param_count,
            distinct = log.entries.len() + 1,
            logged_total = total,
            "the terminal core dropped an escape sequence it does not recognise"
        );
        signal::emit(
            SignalKind::TerminalUnhandledSequence,
            LogFields::new()
                .set("sid", record.identity.session_id.to_string())
                .set("channel_id", record.identity.channel_id.as_u32())
                .set("final", &entry.final_byte)
                .set("private", &entry.private)
                .set("param_count", entry.param_count)
                .set("params", params_text(&entry.params))
                .set("distinct", log.entries.len() + 1)
                .set("logged_total", total)
                .set("ring_dropped", log.ring_dropped)
                // Per-channel scope: a TUI spraying unknown sequences coalesces
                // into one line per cooldown while other sessions stay separate.
                .set(
                    "cooldownKey",
                    record.identity.channel_id.as_u32().to_string(),
                ),
        );
        log.entries.push(entry);
        if log.entries.len() >= UNHANDLED_SEQ_MAX {
            log.capped = true;
            break;
        }
    }
}

/// Sample, then project — so a PARKED pane, which emits no frames, still
/// answers "what did we ignore?" from one snapshot read. `None` means this core
/// has logged nothing, which is not proof of full support.
pub fn unhandled_sequence_snapshot(
    record: &mut SessionRecord,
    now_mono_ms: u64,
) -> Option<UnhandledSequenceSnapshot> {
    note_unhandled_sequences(record, now_mono_ms);
    let log = record.unhandled.as_ref()?;
    Some(UnhandledSequenceSnapshot {
        entries: log
            .entries
            .iter()
            .map(|entry| UnhandledSequenceSnapshotEntry {
                final_byte: entry.final_byte.clone(),
                private: entry.private.clone(),
                param_count: entry.param_count,
                params: entry.params.clone(),
                first_seen_mono_ms: entry.first_seen_mono_ms,
            })
            .collect(),
        logged_total: log.consumed,
        ring_dropped: log.ring_dropped,
        capped: log.capped,
    })
}

fn snapshot_entry(sequence: &UnhandledSequence, now_mono_ms: u64) -> UnhandledSequenceEntry {
    let text = |byte: u8| {
        if byte == 0 {
            String::new()
        } else {
            char::from(byte).to_string()
        }
    };
    UnhandledSequenceEntry {
        final_byte: text(sequence.final_byte),
        private: text(sequence.private),
        param_count: u32::from(sequence.param_count),
        params: sequence
            .recorded_params()
            .iter()
            .map(|&param| u32::from(param))
            .collect(),
        first_seen_mono_ms: now_mono_ms,
    }
}

/// Identity for dedupe. `param_count` is carried apart from `params` because
/// only the first four are recorded: two sequences agreeing on those four can
/// still differ in how many followed.
fn sequence_key(entry: &UnhandledSequenceEntry) -> String {
    format!(
        "{}|{}|{}|{}",
        entry.final_byte,
        entry.private,
        entry.param_count,
        params_text(&entry.params)
    )
}

fn params_text(params: &[u32]) -> String {
    params
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(";")
}
