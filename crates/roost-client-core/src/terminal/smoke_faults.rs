//! Smoke-armed terminal frame faults, fenced to the generation they were armed
//! under: drop every Sync cell frame for a session (blackhole), or drop exactly
//! one non-full frame after it was acknowledged (wire-delta drop). Armed by
//! roost-web's smoke backdoor through `store::sync_smoke`; consumed by
//! `handle_sync::apply_frame` before a frame reaches its replica. Ports the fault
//! half of `apps/web/src/store/terminal-stream-diagnostics.ts:209-267`.

use std::collections::BTreeMap;

use crate::terminal::token::TerminalToken;

/// Which wire shape a frame arrived as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultedFrameKind {
    /// A whole `CellGridFrame`.
    Frame,
    /// One part of a chunked baseline.
    Chunk,
}

/// What the faults did to one session, for the terminal diagnostic snapshot
/// (`faults` in v2's `TerminalStreamDiagnosticsSnapshot`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TerminalFaultCounts {
    /// Frames and chunks the blackhole swallowed.
    pub blackhole_drop_count: u64,
    /// Deltas the one-shot wire drop swallowed.
    pub wire_delta_drop_count: u64,
    /// The sequence of the delta it swallowed.
    pub wire_delta_dropped_seq: Option<u64>,
    /// The first delta sequence seen after the drop.
    pub wire_delta_post_drop_seq: Option<u64>,
}

/// Every armed fault, by session.
#[derive(Debug, Default)]
pub struct TerminalSmokeFaults {
    blackholes: BTreeMap<String, TerminalToken>,
    wire_delta_drops: BTreeMap<String, TerminalToken>,
    counts: BTreeMap<String, TerminalFaultCounts>,
}

impl TerminalSmokeFaults {
    /// Drop every frame for `session_id` while `generation` is the owner.
    pub fn arm_blackhole(&mut self, session_id: &str, generation: TerminalToken) {
        self.blackholes.insert(session_id.to_owned(), generation);
        self.counts
            .entry(session_id.to_owned())
            .or_default()
            .blackhole_drop_count = 0;
        tracing::info!(target: "smoke", session_id, "terminal frame blackhole armed");
    }

    /// Drop the next non-full frame for `session_id` under `generation`.
    pub fn arm_wire_delta_drop(&mut self, session_id: &str, generation: TerminalToken) {
        self.wire_delta_drops
            .insert(session_id.to_owned(), generation);
        let counts = self.counts.entry(session_id.to_owned()).or_default();
        counts.wire_delta_drop_count = 0;
        counts.wire_delta_dropped_seq = None;
        counts.wire_delta_post_drop_seq = None;
        tracing::info!(target: "smoke", session_id, "terminal wire delta drop armed");
    }

    /// Whether the frame must be dropped. A fault armed under another
    /// generation retires here instead of firing.
    pub fn consume(
        &mut self,
        session_id: &str,
        owner: &TerminalToken,
        kind: FaultedFrameKind,
        full: bool,
        seq: Option<u64>,
    ) -> bool {
        if let Some(armed) = self.blackholes.get(session_id) {
            if armed == owner {
                self.counts
                    .entry(session_id.to_owned())
                    .or_default()
                    .blackhole_drop_count += 1;
                return true;
            }
            self.blackholes.remove(session_id);
        }
        if kind != FaultedFrameKind::Frame || full {
            return false;
        }
        if let (Some(seq), Some(counts)) = (seq, self.counts.get_mut(session_id))
            && counts.wire_delta_dropped_seq.is_some()
            && counts.wire_delta_post_drop_seq.is_none()
        {
            counts.wire_delta_post_drop_seq = Some(seq);
        }
        let Some(armed) = self.wire_delta_drops.remove(session_id) else {
            return false;
        };
        if &armed != owner {
            return false;
        }
        let counts = self.counts.entry(session_id.to_owned()).or_default();
        counts.wire_delta_drop_count += 1;
        if seq.is_some() {
            counts.wire_delta_dropped_seq = seq;
        }
        tracing::info!(target: "smoke", session_id, ?seq, "terminal wire delta dropped");
        true
    }

    /// The session's fault counts, zero when none was armed.
    pub fn counts(&self, session_id: &str) -> TerminalFaultCounts {
        self.counts.get(session_id).copied().unwrap_or_default()
    }
}
