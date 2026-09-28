//! The worker's terminal-stream vocabulary and its per-channel generation table:
//! the desire a coordinator or view owner states ([`StreamIntent`]), the one
//! truthful outcome it is told ([`WorkerStreamResult`]), and which generation
//! owns each channel ([`TerminalStreams`]). `session::terminal_control` mints
//! generations, `session::terminal_txn` settles them, `terminal_view` and the
//! pipeline snapshot read them. Ports `apps/worker/src/session/session-terminal-state.ts`
//! and the `terminalStreams`/`lastAppliedSize` maps of `session-manager-state.ts`.
//!
//! WHAT IS NOT HERE. v2's `TerminalStreamState` also carried the per-sink
//! deliveries, the live resize capture and `coreValid`. Those are the emitter's
//! (`session::emit::StreamOutput`) and the delivery's (`runtime::channel_delivery`)
//! in this crate, because a second copy of "is this core trustworthy" is two
//! answers the ingest path and the transaction would disagree on.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use futures_util::future::Shared;
use roost_protocol::wire::brand::{ChannelId, SessionId};
use roost_protocol::wire::coord_worker::{TerminalStreamFailureKind, TerminalWritePhase};

use super::types::SessionRecord;

/// The future every stream transaction resolves through. The same alias as
/// `crate::uplink::OwnerFuture<WorkerStreamResult>`, spelled out so the session
/// layer does not depend on the link.
pub type StreamOperation = Pin<Box<dyn Future<Output = WorkerStreamResult> + Send + 'static>>;

/// v2 `TerminalRequestBudget` (`coord-link-types.ts:65`) as a stream
/// transaction reads it. The coordinator path answers from its link fence and
/// frame-receipt budget; a view owner answers from its own stream table.
pub trait StreamRequestBudget: Send + Sync + std::fmt::Debug {
    /// False once whatever asked can no longer receive the answer.
    fn is_current_connection(&self) -> bool;
    /// v2 `remainingMs() <= 0`.
    fn expired(&self) -> bool;
}

/// The emitter operations a stream transaction performs with the record in
/// hand, reached through `ChannelDelivery::stream_emission` so each runs under
/// the lock the ingest path parses under. `runtime::channel_delivery` is the
/// production implementation; the halves live in `session::emit_streams`.
pub trait StreamEmission: Send + Sync {
    /// v2 `applyTerminalStreamState`'s mint: retire the old generation's
    /// deliveries and queued work, address the emit state to `stream_id`, and
    /// carry core validity across (a fresh channel starts valid).
    fn mint_stream(
        &self,
        record: &mut SessionRecord,
        stream_id: &str,
        enabled: bool,
        geometry_changed: bool,
    );
    /// Whether this channel's core may be parsed and emitted from.
    fn core_valid(&self, channel_id: ChannelId) -> bool;
    /// v2 `installTerminalBaseline`, answering whether the core is still
    /// valid afterwards (an unencodable full latches it invalid).
    fn install_baseline(&self, record: &mut SessionRecord, now_ms: i64) -> bool;
    /// v2 `retireStreamDelivery` + `clearStreamDeliveryDirty`.
    fn retire_delivery(&self, channel_id: ChannelId);
    /// v2 `resetEmissionEpoch`'s delivery half: every sink owes a new baseline.
    fn reset_delivery(&self, channel_id: ChannelId);
    /// v2 `failCore`'s delivery half: retire, latch the core invalid, cancel
    /// queued emission. Later chunks take the retain-only lane.
    fn trap_core(&self, channel_id: ChannelId);
    /// A re-proved core may emit again (v2 `state.coreValid = true`).
    fn prove_core(&self, channel_id: ChannelId);
    /// Queue the probe replies a boundary replay produced for the PTY, AFTER
    /// the resize (v2 `forwardReplies`), on the same lane live replies take.
    fn forward_query_replies(&self, record: &SessionRecord, replies: String);
}

/// One aggregated stream desire (v2 `WorkerTerminalStreamIntent`).
#[derive(Debug, Clone)]
pub struct StreamIntent {
    pub request_id: String,
    pub session_id: SessionId,
    pub stream_id: String,
    pub enabled: bool,
    pub cols: u32,
    pub rows: u32,
    /// Absent for a caller with no deadline, exactly as v2's optional budget.
    pub budget: Option<Arc<dyn StreamRequestBudget>>,
}

/// v2 `WorkerTerminalStreamResult` (`session-terminal-state.ts:16`). A committed
/// result is always phase `written`; the two failures carry their own phase
/// because a keeper refusal after the write is `rejected` + `written`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerStreamResult {
    Committed {
        stream_id: String,
        enabled: bool,
        cols: u32,
        rows: u32,
        channel_resize_seq: u64,
        resized: bool,
    },
    Rejected {
        stream_id: String,
        enabled: bool,
        cols: u32,
        rows: u32,
        channel_resize_seq: u64,
        failure: TerminalStreamFailureKind,
        reason: String,
        phase: TerminalWritePhase,
    },
    Ambiguous {
        stream_id: String,
        enabled: bool,
        cols: u32,
        rows: u32,
        channel_resize_seq: u64,
        failure: TerminalStreamFailureKind,
        reason: String,
        phase: TerminalWritePhase,
    },
}

/// Whether a failure is a rejection or an ambiguity: the two v2 statuses a
/// `failed(...)` result can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailedStatus {
    Rejected,
    Ambiguous,
}

impl WorkerStreamResult {
    /// v2 txn `committed()`: a disabled stream reports zero geometry.
    pub fn committed(facts: StreamFacts<'_>, channel_resize_seq: u64, resized: bool) -> Self {
        let (cols, rows) = if facts.enabled {
            (facts.cols, facts.rows)
        } else {
            (0, 0)
        };
        Self::Committed {
            stream_id: facts.stream_id.to_owned(),
            enabled: facts.enabled,
            cols,
            rows,
            channel_resize_seq,
            resized,
        }
    }

    /// v2 txn `failed()`, with its phase default: a rejection is pre-write and
    /// an ambiguity is unknown unless the caller proved otherwise.
    pub fn failed(
        facts: StreamFacts<'_>,
        channel_resize_seq: u64,
        failure: TerminalStreamFailureKind,
        reason: impl Into<String>,
        status: FailedStatus,
        phase: Option<TerminalWritePhase>,
    ) -> Self {
        let reason = reason.into();
        let stream_id = facts.stream_id.to_owned();
        let (enabled, cols, rows) = (facts.enabled, facts.cols, facts.rows);
        match status {
            FailedStatus::Rejected => Self::Rejected {
                stream_id,
                enabled,
                cols,
                rows,
                channel_resize_seq,
                failure,
                reason,
                phase: phase.unwrap_or(TerminalWritePhase::PreWrite),
            },
            FailedStatus::Ambiguous => Self::Ambiguous {
                stream_id,
                enabled,
                cols,
                rows,
                channel_resize_seq,
                failure,
                reason,
                phase: phase.unwrap_or(TerminalWritePhase::Unknown),
            },
        }
    }

    pub fn stream_id(&self) -> &str {
        match self {
            Self::Committed { stream_id, .. }
            | Self::Rejected { stream_id, .. }
            | Self::Ambiguous { stream_id, .. } => stream_id,
        }
    }

    /// The failure a non-committed result names; `None` for a commit.
    pub fn failure(&self) -> Option<TerminalStreamFailureKind> {
        match self {
            Self::Committed { .. } => None,
            Self::Rejected { failure, .. } | Self::Ambiguous { failure, .. } => Some(*failure),
        }
    }

    pub fn phase(&self) -> TerminalWritePhase {
        match self {
            Self::Committed { .. } => TerminalWritePhase::Written,
            Self::Rejected { phase, .. } | Self::Ambiguous { phase, .. } => *phase,
        }
    }

    pub fn channel_resize_seq(&self) -> u64 {
        match self {
            Self::Committed {
                channel_resize_seq, ..
            }
            | Self::Rejected {
                channel_resize_seq, ..
            }
            | Self::Ambiguous {
                channel_resize_seq, ..
            } => *channel_resize_seq,
        }
    }
}

/// The four facts a failed result echoes: a generation's, or a refused intent's.
#[derive(Debug, Clone, Copy)]
pub struct StreamFacts<'a> {
    pub stream_id: &'a str,
    pub enabled: bool,
    pub cols: u32,
    pub rows: u32,
}

/// The generation that owns one channel (v2 `TerminalStreamState` minus the
/// emitter-owned fields). `version` is the object identity v2 compared with
/// `!==`: a transaction whose generation is no longer current is superseded.
#[derive(Clone)]
pub struct StreamGeneration {
    pub stream_id: String,
    pub enabled: bool,
    pub cols: u32,
    pub rows: u32,
    pub version: u64,
    /// The transaction this generation was minted with. A repeated request for
    /// the same stream id is answered from it, finished or not, as v2 returns
    /// `current.operation`.
    pub(crate) operation: Shared<StreamOperation>,
}

impl StreamGeneration {
    pub fn facts(&self) -> StreamFacts<'_> {
        StreamFacts {
            stream_id: &self.stream_id,
            enabled: self.enabled,
            cols: self.cols,
            rows: self.rows,
        }
    }
}

impl std::fmt::Debug for StreamGeneration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StreamGeneration")
            .field("stream_id", &self.stream_id)
            .field("enabled", &self.enabled)
            .field("cols", &self.cols)
            .field("rows", &self.rows)
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

/// What a reader outside the transaction may know about a channel's stream
/// (v2 readers of `terminalStreams.get(ch)`: the view screen, the pipeline).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalStreamFacts {
    pub version: u64,
    pub stream_id: String,
    pub enabled: bool,
    pub cols: u32,
    pub rows: u32,
    /// Read from the emitter, the one owner of core validity.
    pub core_valid: bool,
}

/// Every channel's current generation, the version counter and the geometry
/// each core was last proven at. One per `SessionManager`.
#[derive(Debug, Default)]
pub struct TerminalStreams {
    table: Mutex<StreamTable>,
}

#[derive(Debug, Default)]
struct StreamTable {
    generations: HashMap<ChannelId, StreamGeneration>,
    last_version: u64,
    applied_size: HashMap<ChannelId, (u16, u16)>,
}

impl TerminalStreams {
    fn table(&self) -> MutexGuard<'_, StreamTable> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn current(&self, channel_id: ChannelId) -> Option<StreamGeneration> {
        self.table().generations.get(&channel_id).cloned()
    }

    /// Whether `version` still owns the channel (v2 `terminalStreams.get(ch) === state`).
    pub fn is_current(&self, channel_id: ChannelId, version: u64) -> bool {
        self.table()
            .generations
            .get(&channel_id)
            .is_some_and(|generation| generation.version == version)
    }

    /// Decide and change one channel's generation atomically. The table lock
    /// is held across `decide`, which takes the record and delivery locks
    /// inside it — so the ORDER is table → record → delivery, and nothing may
    /// touch this table while holding either of those.
    pub(crate) fn transact<R>(
        &self,
        channel_id: ChannelId,
        decide: impl FnOnce(&mut StreamSlot<'_>) -> R,
    ) -> R {
        let mut table = self.table();
        decide(&mut StreamSlot {
            table: &mut table,
            channel_id,
        })
    }

    /// The geometry the core was last proven at (v2 `lastAppliedSize`).
    pub fn applied_size(&self, channel_id: ChannelId) -> Option<(u16, u16)> {
        self.table().applied_size.get(&channel_id).copied()
    }

    pub fn note_applied_size(&self, channel_id: ChannelId, cols: u16, rows: u16) {
        self.table().applied_size.insert(channel_id, (cols, rows));
    }

    /// A closed channel's generation and geometry go with it (v2
    /// `session-lifecycle.ts:252-253`).
    pub fn forget(&self, channel_id: ChannelId) {
        let mut table = self.table();
        let had = table.generations.remove(&channel_id).is_some();
        table.applied_size.remove(&channel_id);
        if had {
            tracing::debug!(%channel_id, "a closed channel's terminal stream generation was forgotten");
        }
    }
}

/// One channel's row of the table, while [`TerminalStreams::transact`] holds it.
pub(crate) struct StreamSlot<'a> {
    table: &'a mut StreamTable,
    channel_id: ChannelId,
}

impl StreamSlot<'_> {
    pub(crate) fn current(&self) -> Option<&StreamGeneration> {
        self.table.generations.get(&self.channel_id)
    }

    pub(crate) fn applied_size(&self) -> Option<(u16, u16)> {
        self.table.applied_size.get(&self.channel_id).copied()
    }

    /// Install the generation `build` makes from the next version (v2
    /// `nextTerminalStreamVersion` + `terminalStreams.set`), and return it.
    pub(crate) fn install(
        &mut self,
        build: impl FnOnce(u64) -> StreamGeneration,
    ) -> StreamGeneration {
        self.table.last_version += 1;
        let generation = build(self.table.last_version);
        self.table
            .generations
            .insert(self.channel_id, generation.clone());
        generation
    }
}
