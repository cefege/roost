//! One Sync v2 terminal lane carrying a live session past its first frame:
//! deltas keep flowing in order after the first one, and view-states queued
//! back to back all reach the client.
//!
//! Ports the delivery cases of `apps/coord/tests/sync/sync-ws-v2-scheduler.test.ts`
//! against `crates/roost-coord/src/sync_ws/terminal/{ready_ring,delivery}.rs`.
//! A lane that sends one delta and then waits forever for a delivery match is
//! the smoke failure "main screen history survives width and height
//! perturbations": the browser sees no deltas, only the fulls its own resync
//! challenges pull, so the history the deltas carry is never painted.

// Every unwrap here is an assertion over a value the test just built: the panic
// IS the failure, which is why `unwrap_used` is denied in product code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::sync::Arc;

use roost_coord::sync_ws::commands::{ClientContext, CommandOutcome, handle_client_frame};
use roost_coord::sync_ws::domain_table::{
    DomainGenerations, TERMINAL_CELL_MAX_RETAINED_FRAMES, TERMINAL_LANE_MAX_DELTA_BYTES,
    TERMINAL_LANE_MAX_DELTA_FRAMES,
};
use roost_coord::sync_ws::egress::FlushStep;
use roost_coord::sync_ws::retained_frame::SharedCellFrame;
use roost_coord::sync_ws::session::SyncV2Session;
use roost_coord::sync_ws::snapshot_registry::SnapshotTokenRegistry;
use roost_coord::sync_ws::terminal::TerminalDeltaOutcome;
use roost_coord::sync_ws::terminal::snapshot::{
    TerminalSnapshotCursor, TerminalSnapshotHub, TerminalSnapshotSource,
};
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::__buffa::oneof::sync_client_frame::Command as ClientCommand;
use roost_proto::{
    FirehoseFrame, PbCellGridFrame, PbCellRow, PbCellSpan, SyncClientFrame, SyncDomain,
    SyncDomainReadyCommand, TerminalViewStateFrame, TerminalViewStatus,
};

const SESSION: &str = "11111111-1111-4111-8111-111111111111";
const STREAM: &str = "stream-target";
const SNAPSHOT: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const SOCKET: &str = "socket-1";

fn grid(seq: u64, full: bool) -> PbCellGridFrame {
    PbCellGridFrame {
        session_id: SESSION.to_owned(),
        stream_id: STREAM.to_owned(),
        cols: 80,
        rows: 24,
        full,
        seq,
        base_seq: seq.saturating_sub(1),
        grid_epoch: "epoch-1".to_owned(),
        viewport_rows: vec![PbCellRow {
            index: 0,
            spans: vec![PbCellSpan {
                text: format!("ROW-{seq}"),
                ..PbCellSpan::default()
            }],
            __buffa_unknown_fields: Default::default(),
        }],
        ..PbCellGridFrame::default()
    }
}

fn delta(seq: u64) -> FirehoseFrame {
    FirehoseFrame {
        frame: Some(Frame::CellGrid(Box::new(grid(seq, false)))),
        ..FirehoseFrame::default()
    }
}

fn view_state(revision: u64) -> FirehoseFrame {
    FirehoseFrame {
        frame: Some(Frame::TerminalViewState(Box::new(TerminalViewStateFrame {
            view_id: "view-1".to_owned(),
            session_id: SESSION.to_owned(),
            revision,
            active: true,
            stream_id: STREAM.to_owned(),
            status: TerminalViewStatus::Accepted.into(),
            effective_cols: 80,
            effective_rows: 24,
            reason: String::new(),
            __buffa_unknown_fields: Default::default(),
        }))),
        ..FirehoseFrame::default()
    }
}

/// A socket whose terminal domain is hydrated and whose lane carries STREAM.
fn hydrated_session() -> SyncV2Session {
    let mut session = SyncV2Session::new(SOCKET.to_owned(), Arc::new(DomainGenerations::new(1_000)), true);
    let mut tokens = SnapshotTokenRegistry::new();
    tokens.register_socket(SOCKET, "fingerprint");
    let covered: BTreeSet<String> = [SESSION.to_owned()].into();
    assert!(tokens.bind(SOCKET, "fingerprint", SNAPSHOT, covered.clone()));
    let ready = SyncClientFrame {
        ack_delivery_seq: None,
        socket_id: SOCKET.to_owned(),
        command: Some(ClientCommand::DomainReady(Box::new(SyncDomainReadyCommand {
            domain: SyncDomain::Terminal.into(),
            generation: session.domain_generation(SyncDomain::Terminal).unwrap(),
            snapshot_token: Some(SNAPSHOT.to_owned()),
            __buffa_unknown_fields: Default::default(),
        }))),
        __buffa_unknown_fields: Default::default(),
    };
    let context = ClientContext {
        read_only: false,
        tab_id: Some("tab-1".to_owned()),
        viewer_key: Some("fingerprint:tab-1".to_owned()),
        fingerprint: "fingerprint".to_owned(),
        session_ids: covered,
    };
    let outcome = handle_client_frame(&mut session, &context, &ready, &mut tokens, 1_000);
    assert!(matches!(outcome, CommandOutcome::DomainReady { domain: SyncDomain::Terminal, .. }));
    assert!(session.begin_terminal_stream(SESSION, STREAM));
    session
}

/// A hub that has no replica: any rebaseline it is asked for is recorded.
#[derive(Default)]
struct RecordingHub {
    requested: Vec<String>,
}

impl TerminalSnapshotHub for RecordingHub {
    fn request_rebaseline(&mut self, _socket_id: &str, session_id: &str) -> bool {
        self.requested.push(session_id.to_owned());
        true
    }
}

/// A canonical full cut into whole-frame parts.
struct CannedSource(Vec<SharedCellFrame>);

impl TerminalSnapshotSource for CannedSource {
    fn create_cursor(&self, _snapshot_id: &str) -> Option<Arc<dyn TerminalSnapshotCursor>> {
        Some(Arc::new(CannedCursor(self.0.clone())))
    }
}

struct CannedCursor(Vec<SharedCellFrame>);

impl TerminalSnapshotCursor for CannedCursor {
    fn part_count(&self) -> u32 {
        u32::try_from(self.0.len()).unwrap()
    }

    fn materialize(&self, part_index: u32) -> Option<SharedCellFrame> {
        self.0.get(usize::try_from(part_index).unwrap()).cloned()
    }
}

/// Everything the socket sends, as the client would read it, acknowledging
/// each frame as it lands so the window never becomes the limit.
fn drain(session: &mut SyncV2Session, hub: &mut RecordingHub) -> Vec<String> {
    let mut sent = Vec::new();
    while let FlushStep::Send(sendable) = session.take_next_sendable(1_000, hub) {
        session.apply_ack(sendable.delivery_seq, 1_000).unwrap();
        sent.push(match &sendable.frame.frame {
            Some(Frame::CellGrid(cell)) => format!("{}-{}", if cell.full { "full" } else { "delta" }, cell.seq),
            Some(Frame::TerminalViewState(state)) => format!("state-{}", state.revision),
            other => format!("{other:?}"),
        });
    }
    sent
}

// v2 "a delta-only lane exposes one head and drains its retained FIFO in order".
#[test]
fn a_delta_only_lane_drains_every_buffered_delta_in_order() {
    let mut session = hydrated_session();
    let mut hub = RecordingHub::default();
    for seq in [11, 12, 13] {
        let outcome = session.enqueue_terminal_delta(SESSION, STREAM, &delta(seq), 1_000, &mut hub);
        assert_eq!(outcome, TerminalDeltaOutcome::Queued);
    }
    assert_eq!(drain(&mut session, &mut hub), ["delta-11", "delta-12", "delta-13"]);
    assert!(hub.requested.is_empty(), "an ordinary stream owes no rebaseline");
}

// v2 "terminal state precedes snapshot parts and delta tail after an ACK restart".
#[test]
fn a_view_state_goes_ahead_of_the_baseline_and_the_deltas_behind_it() {
    let mut session = hydrated_session();
    let mut hub = RecordingHub::default();
    session.enqueue_terminal_state(&view_state(1), SESSION, 1_000, &mut hub).unwrap();
    let source = CannedSource(vec![SharedCellFrame::Full(grid(20, true)), SharedCellFrame::Full(grid(21, false))]);
    assert!(session.replace_terminal_snapshot(SESSION, STREAM, &source, SNAPSHOT, 1_000, &mut hub));
    let outcome = session.enqueue_terminal_delta(SESSION, STREAM, &delta(22), 1_000, &mut hub);
    assert_eq!(outcome, TerminalDeltaOutcome::Queued);
    assert_eq!(drain(&mut session, &mut hub), ["state-1", "full-20", "delta-21", "delta-22"]);
}

// v2 keeps each view-state at the head of its lane until it is delivered
// (`sync-ws-v2-terminal-ready.ts:75-90,175-178`): a second decision queued
// behind the first is the answer the client is waiting for.
#[test]
fn view_states_queued_back_to_back_all_reach_the_client() {
    let mut session = hydrated_session();
    let mut hub = RecordingHub::default();
    for revision in [1, 2, 3] {
        session.enqueue_terminal_state(&view_state(revision), SESSION, 1_000, &mut hub).unwrap();
    }
    assert_eq!(drain(&mut session, &mut hub), ["state-1", "state-2", "state-3"]);
}

// A delta that is delivered releases what it held: a session streaming for
// longer than its lane's byte bound and the socket's whole cell budget runs
// neither dry. Each delta is wide so the byte bound is crossed many times over.
#[test]
fn a_long_stream_never_exhausts_its_lane_or_the_sockets_cell_budget() {
    let mut session = hydrated_session();
    let mut hub = RecordingHub::default();
    let wide = "W".repeat(32 * 1024);
    let lane_bytes_crossings = TERMINAL_LANE_MAX_DELTA_BYTES / 16 / 1024;
    let total = lane_bytes_crossings.max(u64::try_from(TERMINAL_CELL_MAX_RETAINED_FRAMES * 2).unwrap());
    let mut sent = 0;
    for seq in 1..=total {
        let mut frame = delta(seq);
        if let Some(Frame::CellGrid(cell)) = frame.frame.as_mut() {
            cell.viewport_rows[0].spans[0].text.clone_from(&wide);
        }
        let outcome = session.enqueue_terminal_delta(SESSION, STREAM, &frame, 1_000, &mut hub);
        assert_eq!(outcome, TerminalDeltaOutcome::Queued, "delta {seq} was refused");
        sent += drain(&mut session, &mut hub).len();
    }
    assert_eq!(sent, usize::try_from(total).unwrap());
    assert!(!session.terminal_rebaseline_pending(SESSION, STREAM));
    assert!(hub.requested.is_empty());
}

// v2 "a delta-only lane exposes one head and drains its retained FIFO in order"
// keeps the queued head IN the tail, so the delta in flight still counts
// against the lane's frame bound (`sync-ws-v2-terminal.ts:290`).
#[test]
fn the_delta_in_flight_still_counts_against_the_lane_bound() {
    let mut session = hydrated_session();
    let mut hub = RecordingHub::default();
    let bound = u64::try_from(TERMINAL_LANE_MAX_DELTA_FRAMES).unwrap();
    for seq in 1..=bound {
        let outcome = session.enqueue_terminal_delta(SESSION, STREAM, &delta(seq), 1_000, &mut hub);
        assert_eq!(outcome, TerminalDeltaOutcome::Queued, "delta {seq} is inside the bound");
    }
    let outcome = session.enqueue_terminal_delta(SESSION, STREAM, &delta(bound + 1), 1_000, &mut hub);
    assert_ne!(outcome, TerminalDeltaOutcome::Queued);
    assert!(session.terminal_rebaseline_pending(SESSION, STREAM));
}
