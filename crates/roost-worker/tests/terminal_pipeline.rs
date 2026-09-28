//! Worker-owned terminal-pipeline evidence through `PipelineOwner`, the value
//! `DownstreamOwners::pipeline` holds: bounded target and response admission,
//! enum-only reasons, and live reads of the session table and emitter. Ports
//! `apps/worker/tests/terminal/terminal-pipeline-snapshot.test.ts` and the
//! owner case of `apps/worker/tests/transport/coord-link-terminal-pipeline.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod session_support;

use std::sync::{Arc, Mutex};
use std::time::Instant;

use roost_proto::buffa::{EnumValue, Message as _};
use roost_proto::{
    DTerminalPipelineSnapshotRequest, TerminalPipelineReason as Reason,
    TerminalPipelineStage as Stage, TerminalPipelineTarget, WTerminalPipelineSnapshot,
};
use roost_protocol::proto_adapters::coord_worker_proto::encode_upstream;
use roost_protocol::wire::coord_worker::CoordWorkerUpstream;
use roost_worker::link_ports::{LinkPipelineState, TerminalPipelinePort};
use roost_worker::session::emit::CellEmitter;
use roost_worker::terminal_pipeline::bounds::{
    TERMINAL_PIPELINE_MAX_RESPONSE_BYTES, TERMINAL_PIPELINE_MAX_SAFE_INTEGER,
};
use roost_worker::terminal_pipeline::snapshot::KeeperPipelineFacts;
use roost_worker::terminal_pipeline::{KeeperPipelineSource, PipelineOwner};

use session_support::{Harness, OTHER, PinnedClock, SESSION, channel, session_id};

fn reason(value: Reason) -> EnumValue<Reason> {
    EnumValue::from(value)
}

fn stage_value(value: Stage) -> EnumValue<Stage> {
    EnumValue::from(value)
}

const MISSING_SESSION_ID: &str = "55555555-5555-4555-8555-555555555555";

/// The keeper half, fixed: reachable or not, nothing in flight.
#[derive(Debug)]
struct QuietKeeper {
    connected: bool,
}

impl KeeperPipelineSource for QuietKeeper {
    fn keeper_connected(&self) -> bool {
        self.connected
    }
    fn keeper_facts(&self, _channel_id: u16, _now: Instant) -> KeeperPipelineFacts {
        KeeperPipelineFacts::default()
    }
}

struct Fixture {
    harness: Harness,
    emitter: Arc<Mutex<CellEmitter>>,
    owner: PipelineOwner,
}

fn fixture(keeper_connected: bool) -> Fixture {
    let harness = Harness::new();
    let emitter = Arc::new(Mutex::new(CellEmitter::new()));
    let owner = PipelineOwner::new(
        Arc::clone(&harness.table),
        Arc::clone(&harness.manager),
        Arc::clone(&emitter),
        Arc::new(PinnedClock),
        Arc::new(QuietKeeper {
            connected: keeper_connected,
        }),
    );
    Fixture {
        harness,
        emitter,
        owner,
    }
}

fn with_session(fixture: &Fixture, session: &str, channel_id: u16, sequence: u64) {
    fixture.harness.install(
        session,
        channel_id,
        "/home/user/project",
        "/home/user/project",
    );
    fixture
        .harness
        .table
        .with_record_mut(&session_id(session), |record| {
            record.cell_emit.seq = sequence
        })
        .expect("the session was just installed");
}

fn request(targets: &[(&str, &str)]) -> DTerminalPipelineSnapshotRequest {
    DTerminalPipelineSnapshotRequest {
        request_id: "pipeline-request".to_owned(),
        targets: targets
            .iter()
            .map(|(session_id, view_id)| TerminalPipelineTarget {
                session_id: (*session_id).to_owned(),
                view_id: (*view_id).to_owned(),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

const DETACHED: LinkPipelineState = LinkPipelineState {
    queue_frames: 3,
    queue_bytes: 512,
    native_buffered_bytes: 1024,
    attached: false,
};

fn stage_of(
    snapshot: &WTerminalPipelineSnapshot,
    index: usize,
    stage: Stage,
) -> &roost_proto::TerminalPipelineStageSnapshot {
    snapshot.sessions[index]
        .stages
        .iter()
        .find(|candidate| candidate.stage == stage_value(stage))
        .expect("the stage is present")
}

#[test]
fn targets_are_sorted_a_missing_session_is_an_enum_and_known_ones_carry_seven_stages() {
    let fixture = fixture(true);
    with_session(&fixture, SESSION, 51, 11);
    with_session(&fixture, OTHER, 52, 29);
    let snapshot = fixture.owner.pipeline_snapshot(
        request(&[
            (MISSING_SESSION_ID, "view-missing"),
            (OTHER, "view-last"),
            (SESSION, "view-first"),
        ]),
        DETACHED,
    );

    assert_eq!(snapshot.request_id, "pipeline-request");
    let order: Vec<&str> = snapshot
        .sessions
        .iter()
        .map(|session| session.session_id.as_str())
        .collect();
    assert_eq!(order, [SESSION, OTHER, MISSING_SESSION_ID]);
    assert_eq!(snapshot.sessions[0].view_id, "view-first");
    let missing = &snapshot.sessions[2].stages;
    assert_eq!(missing.len(), 1);
    assert_eq!(
        (missing[0].stage, missing[0].reason),
        (
            stage_value(Stage::WorkerStream),
            reason(Reason::SessionNotFound)
        )
    );

    for (index, sequence) in [(0, 11), (1, 29)] {
        let stages = &snapshot.sessions[index].stages;
        assert_eq!(stages.len(), 7);
        assert!(
            stages
                .iter()
                .all(|stage| stage.histogram_buckets.len() <= 16)
        );
        assert!(
            stages.iter().all(|stage| stage.sequence == sequence),
            "sequence is the record's cell seq"
        );
    }
    let link = stage_of(&snapshot, 0, Stage::WorkerCoordLink);
    assert_eq!(link.reason, reason(Reason::CoordLinkUnavailable));
    assert_eq!(
        (
            link.queue_frames,
            link.queue_bytes,
            link.native_buffered_bytes,
            link.count
        ),
        (3, 512, 1024, 3)
    );
    assert_eq!(
        link.generation, 0,
        "no coordinator stream was ever installed"
    );
    assert_eq!(
        stage_of(&snapshot, 0, Stage::WorkerStream).reason,
        reason(Reason::StreamNotFound)
    );
    assert_eq!(stage_of(&snapshot, 0, Stage::WorkerCore).count, 1);
}

#[test]
fn the_scheduler_and_keeper_stages_read_the_live_owners() {
    let fixture = fixture(false);
    with_session(&fixture, SESSION, 51, 1);
    fixture.emitter.lock().unwrap().note_dirty(channel(51));
    let attached = LinkPipelineState {
        attached: true,
        ..DETACHED
    };
    let snapshot = fixture
        .owner
        .pipeline_snapshot(request(&[(SESSION, "view")]), attached);

    let scheduler = stage_of(&snapshot, 0, Stage::WorkerScheduler);
    assert_eq!(
        (scheduler.reason, scheduler.queue_frames),
        (reason(Reason::CellDirty), 1)
    );
    assert_eq!(
        stage_of(&snapshot, 0, Stage::WorkerKeeper).reason,
        reason(Reason::KeeperDisconnected)
    );
    assert_eq!(
        stage_of(&snapshot, 0, Stage::WorkerCoordLink).reason,
        reason(Reason::NativeBuffered)
    );
    assert_eq!(
        stage_of(&snapshot, 0, Stage::WorkerStreamControl).reason,
        reason(Reason::None)
    );
}

#[test]
fn admission_stops_at_sixty_four_targets() {
    let fixture = fixture(true);
    let targets: Vec<(String, String)> = (0..65)
        .map(|index| (format!("missing-{index:02}"), format!("view-{index:02}")))
        .collect();
    let borrowed: Vec<(&str, &str)> = targets
        .iter()
        .map(|(session, view)| (session.as_str(), view.as_str()))
        .collect();
    let snapshot = fixture
        .owner
        .pipeline_snapshot(request(&borrowed), DETACHED);

    assert_eq!(snapshot.sessions.len(), 64);
    assert_eq!((snapshot.dropped_targets, snapshot.dropped_records), (1, 0));
    assert!(
        snapshot
            .sessions
            .iter()
            .all(|session| session.stages[0].reason == reason(Reason::SessionNotFound))
    );
    assert!(snapshot.encoded_len() as usize <= TERMINAL_PIPELINE_MAX_RESPONSE_BYTES);
}

#[test]
fn oversized_records_are_dropped_deterministically_inside_the_encoded_bound() {
    let fixture = fixture(true);
    let padded = |prefix: String, fill: char| -> String {
        prefix
            .chars()
            .chain(std::iter::repeat(fill))
            .take(512)
            .collect()
    };
    let targets: Vec<(String, String)> = (0..64)
        .map(|index| {
            (
                padded(format!("session-{index:03}"), 's'),
                padded(format!("view-{index:03}"), 'v'),
            )
        })
        .collect();
    let borrowed: Vec<(&str, &str)> = targets
        .iter()
        .map(|(session, view)| (session.as_str(), view.as_str()))
        .collect();
    let first = fixture
        .owner
        .pipeline_snapshot(request(&borrowed), DETACHED);
    let second = fixture
        .owner
        .pipeline_snapshot(request(&borrowed), DETACHED);

    assert!(!first.sessions.is_empty() && first.sessions.len() < 64);
    assert_eq!(first.dropped_targets, 0);
    assert!(first.dropped_records > 0);
    assert_eq!(first.sessions.len() as u32 + first.dropped_records, 64);
    let kept: Vec<&str> = first
        .sessions
        .iter()
        .map(|session| session.session_id.as_str())
        .collect();
    let prefix: Vec<&str> = borrowed[..kept.len()]
        .iter()
        .map(|(session, _)| *session)
        .collect();
    assert_eq!(
        kept, prefix,
        "records are kept in sorted order and dropped from the tail"
    );
    assert_eq!(
        second, first,
        "the same request samples the same bounded response"
    );
    assert!(first.encoded_len() as usize <= TERMINAL_PIPELINE_MAX_RESPONSE_BYTES);
    let envelope = encode_upstream(&CoordWorkerUpstream::TerminalPipelineSnapshot(first))
        .expect("the frame encodes");
    assert!(
        envelope.len() <= TERMINAL_PIPELINE_MAX_RESPONSE_BYTES,
        "the envelope is {} bytes",
        envelope.len()
    );
}

#[test]
fn an_oversized_identifier_is_a_dropped_record_and_an_oversized_request_id_is_cleared() {
    let fixture = fixture(true);
    let oversized = "s".repeat(513);
    let mut sampled = request(&[
        (&oversized, "oversized-view"),
        (MISSING_SESSION_ID, "valid-view"),
    ]);
    sampled.request_id = "r".repeat(257);
    let snapshot = fixture.owner.pipeline_snapshot(sampled, DETACHED);

    let ids: Vec<&str> = snapshot
        .sessions
        .iter()
        .map(|session| session.session_id.as_str())
        .collect();
    assert_eq!(ids, [MISSING_SESSION_ID]);
    assert_eq!((snapshot.dropped_targets, snapshot.dropped_records), (0, 1));
    assert_eq!(
        snapshot.request_id, "",
        "a request id past 256 bytes is not echoed"
    );
}

#[test]
fn link_numbers_are_held_to_exact_integers() {
    let fixture = fixture(true);
    with_session(&fixture, SESSION, 51, 1);
    let huge = LinkPipelineState {
        queue_frames: u64::MAX,
        queue_bytes: u64::MAX,
        native_buffered_bytes: 0,
        attached: true,
    };
    let snapshot = fixture
        .owner
        .pipeline_snapshot(request(&[(SESSION, "view")]), huge);

    let link = stage_of(&snapshot, 0, Stage::WorkerCoordLink);
    assert_eq!(link.reason, reason(Reason::None));
    assert_eq!(
        (link.queue_frames, link.queue_bytes, link.count),
        (
            TERMINAL_PIPELINE_MAX_SAFE_INTEGER,
            TERMINAL_PIPELINE_MAX_SAFE_INTEGER,
            TERMINAL_PIPELINE_MAX_SAFE_INTEGER
        )
    );
}
