//! The store slices the Sync registry, presence and control frames fold into
//! that are not plain maps: the routable-seed assembler, the bounded queues the
//! UI drains, and the per-worker probe telemetry.
//!
//! Written only by the folds under `handle_sync/`; read by a host's UI bridge.
//! Ported from `apps/web/src/store/sync-inbound.ts:150-178` (routable chunks),
//! `apps/web/src/store/transport/sync-terminal-control-probe.ts` (telemetry)
//! and `apps/web/src/lib/pairedBrowserNotice.ts` (announcement dedupe).

use std::collections::BTreeSet;

use crate::sync::inbound::RoutableChunk;

/// The newest live audit rows kept for the audit pane: 100, the pane's own
/// page (`AuditLogPane.tsx:27` `PAGE_LIMIT`). The pane loads a page by RPC and
/// prepends live rows; a ring longer than a page would hold rows the pane only
/// shows after it asks for the next page anyway.
pub const AUDIT_ROW_RING_MAX: usize = 100;

/// UI commands held for the UI bridge. v2 dispatches synchronously to the
/// registered bridge and a legacy command with no bridge is a no-op
/// (`uiCommandDispatch.ts:142-155`), so the queue only has to span the gap
/// between a frame and the bridge's next drain; beyond 32 no bridge is
/// draining, and dropping the oldest is v2's no-bridge behaviour for them.
pub const UI_COMMAND_QUEUE_MAX: usize = 32;

/// Presence notices held for the per-session presence handlers. v2 delivers
/// each to a live handler or drops it (`sync-dispatch.ts:18-20`); 128 spans one
/// drain of a busy room — delta and leave notices from several viewers across
/// the panes on screen — and past it no handler is draining.
pub const PRESENCE_NOTICE_QUEUE_MAX: usize = 128;

/// Workers whose probe telemetry is retained, oldest evicted first
/// (`sync-terminal-control-probe.ts:20`).
pub const PROBE_TELEMETRY_WORKERS_MAX: usize = 64;

/// Pairings remembered as already announced, oldest forgotten first
/// (`pairedBrowserNotice.ts:20` `ANNOUNCED_ID_LIMIT`).
pub const ANNOUNCED_PAIRINGS_MAX: usize = 32;

/// The most chunks a retained routable seed may have (`sync-inbound.ts:157`).
pub const ROUTABLE_SEED_CHUNKS_MAX: u32 = 4096;

/// One `session_presence` notice waiting for its session's presence handler.
#[derive(Debug, Clone, PartialEq)]
pub struct PresenceNotice {
    /// The session.
    pub session_id: String,
    /// The opaque payload.
    pub payload: serde_json::Value,
}

/// The newest successful transport probe answer for one worker.
///
/// Fenced to the socket generation it arrived on: v2 keys samples by
/// connection, and a sample from a replaced socket does not describe the
/// replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeTelemetry {
    /// The probe this answered.
    pub request_id: String,
    /// The worker process epoch that answered. Never empty.
    pub worker_epoch: String,
    /// The Sync socket generation that carried the answer.
    pub socket_generation: u64,
    /// When the answer arrived, on the host's clock.
    pub received_at_ms: u64,
}

/// What one routable chunk did to the seed it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutableProgress {
    /// Stored; the seed still has missing chunks, so nothing is published.
    Pending,
    /// The last missing chunk arrived: the whole seed, to replace the set.
    Complete(BTreeSet<String>),
    /// The chunk disagrees with its seed about how many chunks there are. v2
    /// returns false here, and an unapplied frame closes the link
    /// (`sync-inbound.ts:170`, `sync-inbound.ts:74-75`).
    CountMismatch,
}

/// The one chunked routable seed being assembled on this socket.
///
/// At most one: a chunk of a new seed discards any partial one
/// (`chunks.clear()` in `dispatchRoutableChunk`), so a seed abandoned by a
/// redial can never be completed by the next socket's chunks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoutableAssembly {
    pending: Option<PendingRoutableSeed>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingRoutableSeed {
    snapshot_id: String,
    chunks: Vec<Option<Vec<String>>>,
}

impl RoutableAssembly {
    /// No seed in progress.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a seed is partially assembled.
    pub fn is_assembling(&self) -> bool {
        self.pending.is_some()
    }

    /// Forget any partial seed: a new socket or a workers domain reset.
    pub fn clear(&mut self) {
        self.pending = None;
    }

    /// Store one chunk. `chunk` has already passed decode's bounds check, so
    /// `chunk_index < chunk_count <= ROUTABLE_SEED_CHUNKS_MAX`.
    pub fn accept(&mut self, chunk: &RoutableChunk, fps: &[String]) -> RoutableProgress {
        let count = chunk.chunk_count as usize;
        let seed = match &mut self.pending {
            Some(seed) if seed.snapshot_id == chunk.snapshot_id => seed,
            slot => slot.insert(PendingRoutableSeed {
                snapshot_id: chunk.snapshot_id.clone(),
                chunks: vec![None; count],
            }),
        };
        if seed.chunks.len() != count {
            return RoutableProgress::CountMismatch;
        }
        let Some(slot) = seed.chunks.get_mut(chunk.chunk_index as usize) else {
            return RoutableProgress::CountMismatch;
        };
        *slot = Some(fps.to_vec());
        if seed.chunks.iter().any(Option::is_none) {
            return RoutableProgress::Pending;
        }
        let complete = seed.chunks.iter().flatten().flatten().cloned().collect();
        self.pending = None;
        RoutableProgress::Complete(complete)
    }
}
