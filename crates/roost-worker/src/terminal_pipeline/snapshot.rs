//! One bounded, content-free pipeline response from facts the owner has already
//! read: seven stages per known session, one `SESSION_NOT_FOUND` stage per
//! unknown one, and records dropped deterministically until the response fits.
//! Ports `terminalPipelineSnapshot` and its stage/reason helpers from
//! `apps/worker/src/terminal/terminal-pipeline-snapshot.ts`. Called by
//! `terminal_pipeline::PipelineOwner`; depends on `super::bounds`.

use roost_proto::{
    DTerminalPipelineSnapshotRequest, TerminalPipelineReason as Reason,
    TerminalPipelineSessionSnapshot, TerminalPipelineStage as Stage, WTerminalPipelineSnapshot,
};

use super::bounds::{self, StageInput};
use crate::link_ports::LinkPipelineState;
use crate::session::cell_sink::StreamDeliveryAggregate;

/// v2 `TerminalStreamState`, as far as the pipeline reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamPipelineFacts {
    pub generation: u64,
    pub stream_id: String,
    pub enabled: bool,
    pub core_valid: bool,
}

/// Both serialization lanes of one channel. Depths count writers QUEUED behind
/// the holder, as v2's `depth` does; an age is present only while held.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ControlPipelineFacts {
    pub control_depth: u64,
    pub control_running_age_ms: Option<u64>,
    pub admission_depth: u64,
    pub admission_held_age_ms: Option<u64>,
}

/// In-flight keeper writes for one channel (v2 `KeeperPipelineFacts`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeeperPipelineFacts {
    pub input_frames: u64,
    pub input_bytes: u64,
    pub resize_frames: u64,
    pub oldest_age_ms: u64,
    pub histogram_buckets: Vec<u64>,
}

impl KeeperPipelineFacts {
    /// Count one pending write of `age_ms` into the oldest age and histogram.
    pub fn observe_age(&mut self, age_ms: u64) {
        self.oldest_age_ms = self.oldest_age_ms.max(age_ms);
        if self.histogram_buckets.is_empty() {
            self.histogram_buckets = vec![0; bounds::TERMINAL_PIPELINE_MAX_HISTOGRAM_BUCKETS];
        }
        self.histogram_buckets[bounds::histogram_index(age_ms)] += 1;
    }
}

/// Everything one live session's stages read, from one owner pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionPipelineFacts {
    /// The record's `cell_emit.seq`, stamped on every stage.
    pub sequence: u64,
    pub stream: Option<StreamPipelineFacts>,
    pub raw_metadata_frames: u64,
    pub raw_metadata_bytes: u64,
    pub cell_dirty: bool,
    pub pending_repair: bool,
    /// v2 `cellEmissionGates.has`.
    pub cell_gate: bool,
    /// v2 `syncOutputHolds.has`.
    pub sync_output: bool,
    /// How long the current gate suppression has lasted, when there is one.
    pub suppression_age_ms: Option<u64>,
    pub delivery: StreamDeliveryAggregate,
    pub control: ControlPipelineFacts,
    pub keeper: KeeperPipelineFacts,
}

/// Sample every admitted target. `session_facts` answers `None` for a session
/// this worker does not hold.
pub fn terminal_pipeline_snapshot(
    request: &DTerminalPipelineSnapshotRequest,
    link: LinkPipelineState,
    keeper_connected: bool,
    mut session_facts: impl FnMut(&str) -> Option<SessionPipelineFacts>,
) -> WTerminalPipelineSnapshot {
    let admitted = bounds::admit_targets(request);
    let dropped_targets = bounds::dropped_targets(request, admitted.len());
    let link = bounds::normalize_link_state(link);
    let mut response = bounds::response(
        bounds::bounded_request_id(&request.request_id),
        Vec::with_capacity(admitted.len()),
        dropped_targets,
        0,
    );
    for ordered in &admitted {
        let target = ordered.target;
        if !bounds::target_identifiers_are_bounded(target) {
            response.dropped_records =
                bounds::bounded_count(u64::from(response.dropped_records) + 1);
            continue;
        }
        let facts = session_facts(&target.session_id);
        response.sessions.push(TerminalPipelineSessionSnapshot {
            session_id: target.session_id.clone(),
            view_id: target.view_id.clone(),
            stages: match facts {
                Some(facts) => session_stages(facts, keeper_connected, link),
                None => vec![bounds::stage(
                    Stage::WorkerStream,
                    Reason::SessionNotFound,
                    StageInput::default(),
                )],
            },
            ..Default::default()
        });
        if bounds::response_fits(&response) {
            continue;
        }
        response.sessions.pop();
        response.dropped_records = bounds::bounded_count(u64::from(response.dropped_records) + 1);
    }
    // A dropped-record count can itself grow the encoding past the fence.
    while !bounds::response_fits(&response) && response.sessions.pop().is_some() {
        response.dropped_records = bounds::bounded_count(u64::from(response.dropped_records) + 1);
    }
    tracing::debug!(
        request_id = %response.request_id,
        sessions = response.sessions.len(),
        dropped_targets = response.dropped_targets,
        dropped_records = response.dropped_records,
        "a terminal pipeline snapshot was sampled"
    );
    response
}

fn session_stages(
    facts: SessionPipelineFacts,
    keeper_connected: bool,
    link: LinkPipelineState,
) -> Vec<roost_proto::TerminalPipelineStageSnapshot> {
    let stream = facts.stream.as_ref();
    let generation = stream.map_or(0, |stream| stream.generation);
    let stream_id = bounds::bounded_identifier(stream.map_or("", |stream| &stream.stream_id));
    let core_invalid = stream.is_some_and(|stream| !stream.core_valid);
    let base = StageInput {
        generation,
        stream_id,
        sequence: facts.sequence,
        ..StageInput::default()
    };
    let control = facts.control;
    let control_running = u64::from(control.control_running_age_ms.is_some());
    let admission_held = u64::from(control.admission_held_age_ms.is_some());
    let control_ages: Vec<u64> = [
        control.control_running_age_ms,
        control.admission_held_age_ms,
    ]
    .into_iter()
    .flatten()
    .collect();
    let control_queued = control.control_depth + control.admission_depth;
    let control_count = control_queued + control_running + admission_held;
    let scheduled = u64::from(facts.cell_dirty) + u64::from(facts.pending_repair);
    let keeper = facts.keeper;
    let keeper_frames = keeper.input_frames + keeper.resize_frames;
    let raw_pending = facts.raw_metadata_frames > 0;
    vec![
        bounds::stage(
            Stage::WorkerPty,
            if raw_pending {
                Reason::RawMetadataPending
            } else {
                Reason::None
            },
            StageInput {
                queue_frames: facts.raw_metadata_frames,
                queue_bytes: facts.raw_metadata_bytes,
                count: facts.raw_metadata_frames,
                ..base.clone()
            },
        ),
        bounds::stage(
            Stage::WorkerCore,
            if core_invalid {
                Reason::CoreInvalid
            } else {
                Reason::None
            },
            // A live record always holds its core; v2 counts `wtermCore ? 1 : 0`.
            StageInput {
                count: 1,
                ..base.clone()
            },
        ),
        bounds::stage(
            Stage::WorkerScheduler,
            scheduler_reason(
                facts.sync_output,
                facts.cell_gate,
                facts.pending_repair,
                facts.cell_dirty,
            ),
            StageInput {
                queue_frames: scheduled,
                count: scheduled,
                oldest_age_ms: facts.suppression_age_ms.unwrap_or(0),
                histogram_buckets: facts
                    .suppression_age_ms
                    .map_or_else(Vec::new, |age| bounds::histogram_for_ages(&[age])),
                ..base.clone()
            },
        ),
        bounds::stage(
            Stage::WorkerStream,
            stream_reason(stream, &facts.delivery, facts.pending_repair),
            StageInput {
                queue_frames: facts.delivery.remaining_snapshot_parts as u64,
                count: facts.delivery.snapshot_part_count as u64,
                ..base.clone()
            },
        ),
        bounds::stage(
            Stage::WorkerStreamControl,
            if control_count > 0 {
                Reason::ControlQueued
            } else {
                Reason::None
            },
            StageInput {
                queue_frames: control_queued,
                oldest_age_ms: bounds::oldest_age(&control_ages),
                count: control_count,
                histogram_buckets: bounds::histogram_for_ages(&control_ages),
                ..base.clone()
            },
        ),
        bounds::stage(
            Stage::WorkerKeeper,
            keeper_reason(keeper_connected, keeper.input_frames, keeper.resize_frames),
            StageInput {
                queue_frames: keeper_frames,
                queue_bytes: keeper.input_bytes,
                oldest_age_ms: keeper.oldest_age_ms,
                count: keeper_frames,
                histogram_buckets: keeper.histogram_buckets,
                ..base.clone()
            },
        ),
        bounds::stage(
            Stage::WorkerCoordLink,
            link_reason(link),
            StageInput {
                queue_frames: link.queue_frames,
                queue_bytes: link.queue_bytes,
                native_buffered_bytes: link.native_buffered_bytes,
                count: link.queue_frames,
                ..base
            },
        ),
    ]
}

fn scheduler_reason(
    sync_output: bool,
    cell_gate: bool,
    pending_repair: bool,
    cell_dirty: bool,
) -> Reason {
    if sync_output {
        Reason::SyncOutput
    } else if cell_gate {
        Reason::CellGate
    } else if pending_repair {
        Reason::PendingRepair
    } else if cell_dirty {
        Reason::CellDirty
    } else {
        Reason::None
    }
}

fn stream_reason(
    stream: Option<&StreamPipelineFacts>,
    delivery: &StreamDeliveryAggregate,
    pending_repair: bool,
) -> Reason {
    let Some(stream) = stream else {
        return Reason::StreamNotFound;
    };
    if !stream.enabled {
        Reason::StreamDisabled
    } else if !stream.core_valid {
        Reason::CoreInvalid
    } else if delivery.snapshot_pending {
        Reason::SnapshotPending
    } else if delivery.active_sinks > 0 && !delivery.baseline_ready {
        Reason::BaselinePending
    } else if pending_repair {
        Reason::PendingRepair
    } else {
        Reason::None
    }
}

fn keeper_reason(connected: bool, input_frames: u64, resize_frames: u64) -> Reason {
    if !connected {
        Reason::KeeperDisconnected
    } else if resize_frames > 0 {
        Reason::KeeperResizePending
    } else if input_frames > 0 {
        Reason::KeeperInputPending
    } else {
        Reason::None
    }
}

fn link_reason(link: LinkPipelineState) -> Reason {
    if !link.attached {
        Reason::CoordLinkUnavailable
    } else if link.native_buffered_bytes > 0 {
        Reason::NativeBuffered
    } else {
        Reason::None
    }
}
