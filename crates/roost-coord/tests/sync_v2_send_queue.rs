//! The Sync v2 send queue: the order it picks frames in, the bounds that make it
//! drop or reset, and the age override that stops a streaming terminal from
//! starving every other domain.
//!
//! These are the rules a live socket cannot be asked about reliably: a frame
//! held behind a fence, a queue at 512 frames, and a lane that has waited past
//! 100 ms are all states a browser reaches in under a second and a test
//! reproduces exactly.

// Every unwrap here is an assertion over a value the test just built: the panic
// IS the failure, which is why `unwrap_used` is denied in product code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::sync::Arc;

use roost_coord::sync_ws::admission::EnqueueOutcome;
use roost_coord::sync_ws::commands::{ClientContext, CommandOutcome, handle_client_frame};
use roost_coord::sync_ws::domain_table::DomainGenerations;
use roost_coord::sync_ws::domain_table::{DOMAIN_MAX_QUEUED_FRAMES, LOW_LANE_MAX_AGE_MS};
use roost_coord::sync_ws::egress::FlushStep;
use roost_coord::sync_ws::frame_meta::{FeedLane, SyncFrameMeta, frame_meta_for};
use roost_coord::sync_ws::session::SyncV2Session;
use roost_coord::sync_ws::snapshot_registry::SnapshotTokenRegistry;
use roost_coord::sync_ws::terminal::snapshot::{NoTerminalSnapshotHub, TerminalSnapshotHub};
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::__buffa::oneof::session_event_proto::Kind;
use roost_proto::__buffa::oneof::sync_client_frame::Command as ClientCommand;
use roost_proto::{
    FirehoseFrame, OpenedEvt, PbCellGridFrame, PbCellRow, PbCellSpan, SessionEventProto,
    SyncClientFrame, SyncDomain, SyncDomainReadyCommand,
};

const SESSION_A: &str = "11111111-1111-4111-8111-111111111111";
const SNAPSHOT_A: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

fn generations() -> Arc<DomainGenerations> {
    Arc::new(DomainGenerations::new(1_000))
}

fn context() -> ClientContext {
    let mut session_ids = BTreeSet::new();
    session_ids.insert(SESSION_A.to_owned());
    ClientContext {
        read_only: false,
        tab_id: Some("tab-1".to_owned()),
        viewer_key: Some("fingerprint:tab-1".to_owned()),
        fingerprint: "fingerprint".to_owned(),
        session_ids,
    }
}

/// The hub the session calls back into when a lane rebaselines.
#[derive(Default)]
#[allow(dead_code)]
struct RecordingHub;

impl TerminalSnapshotHub for RecordingHub {
    fn request_rebaseline(&mut self, _socket_id: &str, _session_id: &str) -> bool {
        true
    }
}

fn cell_frame(session_id: &str, marker: &str, seq: u64) -> FirehoseFrame {
    FirehoseFrame {
        frame: Some(Frame::CellGrid(Box::new(PbCellGridFrame {
            session_id: session_id.to_owned(),
            cols: 80,
            rows: 24,
            seq,
            grid_epoch: format!("{session_id}:grid"),
            viewport_rows: vec![PbCellRow {
                index: 0,
                spans: vec![PbCellSpan {
                    text: marker.to_owned(),
                    ..PbCellSpan::default()
                }],
                __buffa_unknown_fields: Default::default(),
            }],
            ..PbCellGridFrame::default()
        }))),
        ..FirehoseFrame::default()
    }
}

fn opened_event(session_id: &str) -> FirehoseFrame {
    FirehoseFrame {
        frame: Some(Frame::SessionEvent(Box::new(SessionEventProto {
            kind: Some(Kind::Opened(Box::new(OpenedEvt {
                session_id: session_id.to_owned(),
                worker_fp: "worker-1".to_owned(),
                channel: 0,
                ..OpenedEvt::default()
            }))),
            event_id: 1,
            __buffa_unknown_fields: Default::default(),
        }))),
        ..FirehoseFrame::default()
    }
}

fn meta_of(frame: &FirehoseFrame) -> SyncFrameMeta {
    frame_meta_for(frame.frame.as_ref().expect("a test frame carries a oneof"))
}

fn hydrated_terminal() -> (SyncV2Session, SnapshotTokenRegistry) {
    let mut session = SyncV2Session::new("socket-1".to_owned(), generations(), true);
    let mut tokens = SnapshotTokenRegistry::new();
    tokens.register_socket("socket-1", "fingerprint");
    let mut covered = BTreeSet::new();
    covered.insert(SESSION_A.to_owned());
    assert!(tokens.bind("socket-1", "fingerprint", SNAPSHOT_A, covered));
    let frame = SyncClientFrame {
        ack_delivery_seq: None,
        socket_id: "socket-1".to_owned(),
        command: Some(ClientCommand::DomainReady(Box::new(
            SyncDomainReadyCommand {
                domain: SyncDomain::Terminal.into(),
                generation: session
                    .domain_generation(SyncDomain::Terminal)
                    .expect("a domain exists"),
                snapshot_token: Some(SNAPSHOT_A.to_owned()),
                __buffa_unknown_fields: Default::default(),
            },
        ))),
        __buffa_unknown_fields: Default::default(),
    };
    let context = context();
    assert!(matches!(
        handle_client_frame(&mut session, &context, &frame, &mut tokens, 1_000),
        CommandOutcome::DomainReady {
            domain: SyncDomain::Terminal,
            ..
        }
    ));
    (session, tokens)
}

#[test]
fn the_queue_drops_one_frame_and_resets_the_domain_rather_than_growing() {
    let (mut session, _tokens) = hydrated_terminal();
    let mut hub = NoTerminalSnapshotHub;
    // A non-terminal domain overflows into a reset, so the test uses the workers
    // domain, which needs no token.
    let workers_generation = session
        .domain_generation(SyncDomain::Workers)
        .expect("a domain exists");
    let context = context();
    let ready = handle_client_frame(
        &mut session,
        &context,
        &SyncClientFrame {
            ack_delivery_seq: None,
            socket_id: "socket-1".to_owned(),
            command: Some(ClientCommand::DomainReady(Box::new(
                SyncDomainReadyCommand {
                    domain: SyncDomain::Workers.into(),
                    generation: workers_generation,
                    snapshot_token: None,
                    __buffa_unknown_fields: Default::default(),
                },
            ))),
            __buffa_unknown_fields: Default::default(),
        },
        &mut SnapshotTokenRegistry::new(),
        1_000,
    );
    assert!(matches!(
        ready,
        CommandOutcome::DomainReady {
            domain: SyncDomain::Workers,
            ..
        }
    ));
    let worker_meta = SyncFrameMeta {
        domain: Some(SyncDomain::Workers),
        lane: FeedLane::Nonterminal,
        ..SyncFrameMeta::default()
    };
    let generation = session
        .domain_generation(SyncDomain::Workers)
        .expect("a domain exists");
    for index in 0..DOMAIN_MAX_QUEUED_FRAMES {
        let frame = cell_frame(
            SESSION_A,
            "marker",
            u64::try_from(index).unwrap_or_default(),
        );
        let meta = SyncFrameMeta {
            domain: Some(SyncDomain::Workers),
            lane: FeedLane::Nonterminal,
            terminal_stream_id: Some(format!("{generation}")),
            ..SyncFrameMeta::default()
        };
        assert!(
            session
                .enqueue_frame(&frame, Some(&meta), 1_000, &mut hub)
                .is_queued(),
            "frame {index} of the domain's own budget must be admitted"
        );
    }
    let overflow = cell_frame(SESSION_A, "overflow", 9_999);
    let outcome = session.enqueue_frame(&overflow, Some(&worker_meta), 1_000, &mut hub);
    let EnqueueOutcome::Reset(notice) = outcome else {
        panic!("a full non-terminal domain resets instead of growing");
    };
    assert_eq!(notice.domain, SyncDomain::Workers);
    assert_ne!(
        notice.generation, generation,
        "a reset mints a new generation"
    );
    assert!(!notice.terminal_sessions_dropped);
}

// UNFINISHED, and the fault is NOT the one this attribute used to name. It is
// not `terminal/ready_ring.rs::pump_lane`: the ready ring is EMPTY for the whole
// of this test, because `enqueue_frame` puts a frame straight into its domain's
// queue and never creates a terminal lane, so `pump_lane` is never entered and
// no change to it can move this test. Measured: the failure is on the FIRST
// `take_next_sendable`, at the assertion that the announcement is the frame
// selected -- before the acknowledgement this test is about ever happens.
//
// THE CAUSE IS THE FIXTURE, NOT THE FENCE. `hydrated_terminal` binds the
// snapshot token over SESSION_A, and `handle_domain_ready` seeds
// `announced_sessions` from the covered set -- exactly as v2 does
// (`sync-ws-v2-commands.ts:127-130`). So by the time this test queues its cell,
// the session has already been announced and `is_eligible`
// (`send_queue.rs:249`) lets the cell straight through: the cell goes out
// first, and the assertion that the announcement is selected first cannot hold.
//
// The fence is correct. Measured: with the token covering nothing -- so
// hydration announces nobody -- this test passes with every assertion
// unchanged. Emptying the covered set is NOT the fix, because it would stop
// this test exercising hydration at all, which is the state a real socket is
// in. What is needed is a second fixture that hydrates a socket which is
// watching a session the seed did not cover.
#[ignore = "UNFINISHED: the hydrated socket has already announced SESSION_A (handle_domain_ready seeds announced_sessions from the token's covered set, as v2 does), so the cell is never fenced and goes out first. The fence is correct: with the token covering nothing this test passes unchanged. Not a ready-ring fault -- the ring is empty throughout."]
#[test]
fn a_cell_is_fenced_behind_its_announcement_until_the_acknowledgement_lands() {
    let (mut session, _tokens) = hydrated_terminal();
    let mut hub = NoTerminalSnapshotHub;

    let opened = opened_event(SESSION_A);
    let cell = cell_frame(SESSION_A, "MARKER", 1);
    assert!(matches!(
        session.enqueue_frame(&cell, Some(&meta_of(&cell)), 1_000, &mut hub),
        EnqueueOutcome::Queued
    ));
    assert!(matches!(
        session.enqueue_frame(&opened, Some(&meta_of(&opened)), 1_000, &mut hub),
        EnqueueOutcome::Queued
    ));

    // The cell queued FIRST, and it still may not go out: the session has not
    // been announced.
    let first = session.take_next_sendable(1_000, &mut hub);
    let FlushStep::Send(first) = first else {
        panic!("the announcement is eligible and must be selected first");
    };
    assert!(matches!(first.frame.frame, Some(Frame::SessionEvent(_))));

    // With the announcement sent but not acknowledged, the cell is still held.
    assert!(matches!(
        session.take_next_sendable(1_000, &mut hub),
        FlushStep::Idle
    ));

    // Once the client acknowledges the announcement, the cell flows.
    session
        .apply_ack(first.delivery_seq, 1_000)
        .expect("a valid ack");
    let second = session.take_next_sendable(1_000, &mut hub);
    let FlushStep::Send(second) = second else {
        panic!("an acknowledged announcement unfences the session's cells");
    };
    assert!(matches!(second.frame.frame, Some(Frame::CellGrid(_))));
}

// UNFINISHED, and it has TWO independent faults, neither of them the one this
// attribute used to name. It is not `terminal/ready_ring.rs::pump_lane`: the
// ready ring is empty throughout, because `enqueue_frame` never creates a
// terminal lane, so `pump_lane` is never entered.
//
// FAULT ONE, the same as the sibling test: the hydrated socket has already
// announced SESSION_A, so the cell at index 0 is eligible and the first
// `take_next_sendable` returns it, so the test fails on its `TerminalTitle`
// assertion before the claim under test is even reached.
//
// FAULT TWO is the load-bearing one, and it survives that. Measured: with the
// fixture corrected so the first cell is genuinely fenced, the test advances
// past its first assertion and then fails on the aged-out override. The queue
// at that point is [cell(fenced), opened@2000, cell2(fenced), last_activity@
// 2000]. `select_candidate` (`send_queue.rs:83-94`) considers only the FIRST
// ELIGIBLE frame per domain and the age override then takes the oldest of
// those heads, so the head is `opened` -- and `opened` and `last_activity`
// carry the SAME `queued_at_ms`, which the oldest-wins tiebreak resolves in
// favour of the earlier one. v2 selects identically
// (`sync-ws-v2-queue.ts:77-95`), so no FIFO-among-equal-timestamps rule can
// prefer the later frame and this assertion is not satisfiable as written in
// either implementation. It needs either a distinct `queued_at_ms` for the
// `LastActivity`, or a selection rule that scans past an eligible frame to
// find the oldest AGED one -- and the latter is a policy change to
// `select_candidate`, not a port of v2.
#[ignore = "UNFINISHED: two faults. (1) the hydrated socket already announced SESSION_A, so the first cell is never fenced and the title assertion fails; (2) with that corrected the aged-out override still cannot pick the LastActivity, because select_candidate only considers each domain's first eligible frame and opened shares its queued_at_ms -- v2 behaves identically (sync-ws-v2-queue.ts:77-95). Not a ready-ring fault: the ring is empty throughout."]
#[test]
fn the_aged_out_lane_outranks_a_streaming_terminal() {
    let (mut session, _tokens) = hydrated_terminal();
    let mut hub = NoTerminalSnapshotHub;

    // A cell for a session nobody announced stays queued and ineligible.
    let cell = cell_frame(SESSION_A, "MARKER", 1);
    session.enqueue_frame(&cell, Some(&meta_of(&cell)), 1_000, &mut hub);
    // A title for the same session is eligible immediately.
    let title = FirehoseFrame {
        frame: Some(Frame::TerminalTitle(Box::new(
            roost_proto::TerminalTitleFrame {
                session_id: SESSION_A.to_owned(),
                title: "vim".to_owned(),
                __buffa_unknown_fields: Default::default(),
            },
        ))),
        ..FirehoseFrame::default()
    };
    session.enqueue_frame(&title, Some(&meta_of(&title)), 1_000, &mut hub);

    let immediate = session.take_next_sendable(1_000, &mut hub);
    let FlushStep::Send(immediate) = immediate else {
        panic!("the eligible non-cell frame must be selected");
    };
    assert!(matches!(
        immediate.frame.frame,
        Some(Frame::TerminalTitle(_))
    ));

    // A cell for an ANNOUNCED session that has waited past the age bound is
    // overtaken by a non-cell frame queued behind it.
    let opened = opened_event(SESSION_A);
    session.enqueue_frame(&opened, Some(&meta_of(&opened)), 2_000, &mut hub);
    let cell2 = cell_frame(SESSION_A, "LATE", 2);
    session.enqueue_frame(&cell2, Some(&meta_of(&cell2)), 2_000, &mut hub);
    let late_title = FirehoseFrame {
        frame: Some(Frame::LastActivity(Box::new(
            roost_proto::LastActivityFrame {
                session_id: SESSION_A.to_owned(),
                ts_ms: 1.0,
                __buffa_unknown_fields: Default::default(),
            },
        ))),
        ..FirehoseFrame::default()
    };
    session.enqueue_frame(&late_title, Some(&meta_of(&late_title)), 2_000, &mut hub);
    let aged = 2_000 + LOW_LANE_MAX_AGE_MS;
    let next = session.take_next_sendable(aged, &mut hub);
    let FlushStep::Send(next) = next else {
        panic!("a frame is eligible");
    };
    assert!(
        matches!(next.frame.frame, Some(Frame::LastActivity(_))),
        "the aged-out non-cell lane outranks a fenced cell"
    );
    assert_eq!(
        roost_coord::sync_ws::domain_table::DOMAIN_SLOTS,
        7,
        "one slot per Sync domain"
    );
}
