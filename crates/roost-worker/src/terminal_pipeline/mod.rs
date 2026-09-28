//! The worker's terminal-pipeline evidence owner: it answers one
//! `DTerminalPipelineSnapshotRequest` by reading live session, emitter, lane and
//! keeper state, never terminal content. Ports the owner half of
//! `apps/worker/src/terminal/terminal-pipeline-snapshot.ts` (`indexSessions`,
//! `sampleKeeperFacts`, the per-session reads). `runtime::owners` builds it
//! into `DownstreamOwners::pipeline`; the link dispatcher calls it.

pub mod bounds;
pub mod snapshot;

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use roost_observability::clock::EventClock;
use roost_proto::{DTerminalPipelineSnapshotRequest, WTerminalPipelineSnapshot};
use roost_protocol::wire::brand::{ChannelId, SessionId};

use self::snapshot::{
    ControlPipelineFacts, KeeperPipelineFacts, SessionPipelineFacts, StreamPipelineFacts,
    terminal_pipeline_snapshot,
};
use crate::keeper_pool::{KeeperPool, PendingInputUsage};
use crate::link_ports::{LinkPipelineState, TerminalPipelinePort};
use crate::session::control_lanes::{LaneSnapshot, mono_ms};
use crate::session::emit::CellEmitter;
use crate::session::lifecycle::{SessionManager, SessionTable};

/// The keeper half of the evidence: whether the keeper is reachable, and the
/// writes still in flight on one channel. `KeeperPool` is the production source.
pub trait KeeperPipelineSource: Send + Sync + std::fmt::Debug {
    fn keeper_connected(&self) -> bool;
    fn keeper_facts(&self, channel_id: u16, now: Instant) -> KeeperPipelineFacts;
}

impl KeeperPipelineSource for KeeperPool {
    fn keeper_connected(&self) -> bool {
        self.is_connected()
    }

    fn keeper_facts(&self, channel_id: u16, now: Instant) -> KeeperPipelineFacts {
        keeper_facts_from_pending(
            &self.pending_input(channel_id),
            &self.pending_resize_starts(channel_id),
            now,
        )
    }
}

/// v2 `sampleKeeperFacts` for one channel: every pending input and every
/// pending resize is one frame aged from its start, and the usage ledger can
/// only raise the input frame count (`terminal-pipeline-snapshot.ts:160-180`).
pub fn keeper_facts_from_pending(
    pending: &PendingInputUsage,
    resize_starts: &[Instant],
    now: Instant,
) -> KeeperPipelineFacts {
    let mut facts = KeeperPipelineFacts::default();
    for started in &pending.started {
        facts.input_frames += 1;
        facts.observe_age(age_ms(now.saturating_duration_since(*started).as_millis()));
    }
    for started in resize_starts {
        facts.resize_frames += 1;
        facts.observe_age(age_ms(now.saturating_duration_since(*started).as_millis()));
    }
    facts.input_frames = facts.input_frames.max(u64::from(pending.commands));
    facts.input_bytes = bounds::nonnegative_integer(pending.bytes);
    facts
}

/// `DownstreamOwners::pipeline`: samples synchronously, so every target of one
/// request sees one pass over the owners.
#[derive(Debug)]
pub struct PipelineOwner {
    table: Arc<SessionTable>,
    manager: Arc<SessionManager>,
    emitter: Arc<Mutex<CellEmitter>>,
    clock: Arc<dyn EventClock>,
    keeper: Arc<dyn KeeperPipelineSource>,
}

impl PipelineOwner {
    /// `emitter` is the ONE `CellEmitter` the session stack's deliveries share
    /// (`TableCellDelivery::emitter`); `clock` is the clock that stamps its gates.
    pub fn new(
        table: Arc<SessionTable>,
        manager: Arc<SessionManager>,
        emitter: Arc<Mutex<CellEmitter>>,
        clock: Arc<dyn EventClock>,
        keeper: Arc<dyn KeeperPipelineSource>,
    ) -> Self {
        Self {
            table,
            manager,
            emitter,
            clock,
            keeper,
        }
    }

    fn session_facts(&self, session_id: &str) -> Option<SessionPipelineFacts> {
        let session_id = SessionId::try_from(session_id).ok()?;
        let raw_channel = self.table.channel_of(&session_id)?;
        let sequence = self
            .table
            .with_channel_record(raw_channel, |record| record.cell_emit.seq)?;
        let channel_id = ChannelId::try_from(i64::from(raw_channel)).ok()?;
        let stream = self
            .manager
            .terminal_stream_facts(channel_id)
            .map(|stream| StreamPipelineFacts {
                generation: stream.version,
                stream_id: stream.stream_id,
                enabled: stream.enabled,
                core_valid: stream.core_valid,
            });
        let control = control_facts(self.manager.control_lanes().snapshot(channel_id), mono_ms());
        let keeper = self.keeper.keeper_facts(raw_channel, Instant::now());
        let now_ms = self.clock.now_epoch_ms();
        let emitter = self.emitter.lock().unwrap_or_else(PoisonError::into_inner);
        let (raw_frames, raw_bytes) = emitter.raw_metadata().channel_backlog(channel_id);
        Some(SessionPipelineFacts {
            sequence,
            stream,
            raw_metadata_frames: raw_frames as u64,
            raw_metadata_bytes: raw_bytes as u64,
            cell_dirty: emitter.is_dirty(channel_id),
            pending_repair: emitter
                .streams
                .get(&channel_id)
                .is_some_and(|output| output.pending_repair),
            cell_gate: emitter.gate_held(channel_id),
            sync_output: emitter.sync_output_held(channel_id),
            suppression_age_ms: emitter.gate_suppression(channel_id).map(|suppression| {
                age_ms(now_ms.saturating_sub(suppression.since_ms).max(0) as u128)
            }),
            delivery: emitter.delivery_aggregate(channel_id),
            control,
            keeper,
        })
    }
}

impl TerminalPipelinePort for PipelineOwner {
    fn pipeline_snapshot(
        &self,
        request: DTerminalPipelineSnapshotRequest,
        link: LinkPipelineState,
    ) -> WTerminalPipelineSnapshot {
        let keeper_connected = self.keeper.keeper_connected();
        terminal_pipeline_snapshot(&request, link, keeper_connected, |session_id| {
            self.session_facts(session_id)
        })
    }
}

/// v2 lane depth counts writers QUEUED behind the holder; a Rust lane's depth
/// also counts the holder, so it is taken back out here.
pub fn control_facts(lanes: LaneSnapshot, now_mono_ms: u64) -> ControlPipelineFacts {
    let control_running = lanes.control_running.is_some();
    let admission_held = lanes.admission_holder.is_some();
    ControlPipelineFacts {
        control_depth: u64::from(
            lanes
                .control_depth
                .saturating_sub(u32::from(control_running)),
        ),
        control_running_age_ms: control_running.then(|| {
            age_ms(u128::from(
                now_mono_ms.saturating_sub(lanes.control_running_since_ms),
            ))
        }),
        admission_depth: u64::from(
            lanes
                .admission_depth
                .saturating_sub(u32::from(admission_held)),
        ),
        admission_held_age_ms: admission_held.then(|| {
            age_ms(u128::from(
                now_mono_ms.saturating_sub(lanes.admission_held_since_ms),
            ))
        }),
    }
}

fn age_ms(elapsed_ms: u128) -> u64 {
    bounds::nonnegative_integer(u64::try_from(elapsed_ms).unwrap_or(u64::MAX))
}
