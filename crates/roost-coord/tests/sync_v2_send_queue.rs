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

mod sync_v2_send_queue_support;

use roost_coord::sync_ws::admission::EnqueueOutcome;
use roost_coord::sync_ws::commands::{CommandOutcome, handle_client_frame};
use roost_coord::sync_ws::domain_table::{DOMAIN_MAX_QUEUED_FRAMES, LOW_LANE_MAX_AGE_MS};
use roost_coord::sync_ws::egress::FlushStep;
use roost_coord::sync_ws::frame_meta::{FeedLane, SyncFrameMeta};
use roost_coord::sync_ws::snapshot_registry::SnapshotTokenRegistry;
use roost_coord::sync_ws::terminal::snapshot::NoTerminalSnapshotHub;
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::__buffa::oneof::sync_client_frame::Command as ClientCommand;
use roost_proto::{FirehoseFrame, SyncClientFrame, SyncDomain, SyncDomainReadyCommand};
use sync_v2_send_queue_support::{
    SESSION_A, cell_frame, context, hydrated_terminal, hydrated_uncovered_terminal, meta_of,
    opened_event,
};

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

/// Runs on the uncovered fixture: on `hydrated_terminal` the seed has already
/// announced `SESSION_A`, so its cell would never be fenced at all.
#[test]
fn a_cell_is_fenced_behind_its_announcement_until_the_acknowledgement_lands() {
    let (mut session, _tokens) = hydrated_uncovered_terminal();
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

/// The age override ranks domain HEADS only, as v2 does (`sync-ws-v2-queue.ts:72-95`):
/// within one domain a non-cell frame waits behind the frames queued ahead of it, so
/// the aged `LastActivity` goes out after the `LATE` cell enqueued before it.
#[test]
fn an_aged_non_cell_waits_behind_eligible_frames_in_its_own_domain() {
    let (mut session, _tokens) = hydrated_uncovered_terminal();
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

    // The announcement, a cell, and a non-cell queued behind that cell, all in
    // the terminal domain and all past the age bound when the drain runs.
    let opened = opened_event(SESSION_A);
    session.enqueue_frame(&opened, Some(&meta_of(&opened)), 2_000, &mut hub);
    let late_cell = cell_frame(SESSION_A, "LATE", 2);
    session.enqueue_frame(&late_cell, Some(&meta_of(&late_cell)), 2_000, &mut hub);
    let last_activity = FirehoseFrame {
        frame: Some(Frame::LastActivity(Box::new(
            roost_proto::LastActivityFrame {
                session_id: SESSION_A.to_owned(),
                ts_ms: 1.0,
                __buffa_unknown_fields: Default::default(),
            },
        ))),
        ..FirehoseFrame::default()
    };
    session.enqueue_frame(
        &last_activity,
        Some(&meta_of(&last_activity)),
        2_000,
        &mut hub,
    );

    // Drain as a client would, acknowledging each frame so the announcement
    // releases the session's cells, until the `LastActivity` goes out.
    let aged = 2_000 + LOW_LANE_MAX_AGE_MS;
    let mut sent = Vec::new();
    for _ in 0..16 {
        let FlushStep::Send(next) = session.take_next_sendable(aged, &mut hub) else {
            panic!("the drain stalled before the LastActivity was sent; sent {sent:?}");
        };
        session
            .apply_ack(next.delivery_seq, aged)
            .expect("a valid ack");
        let reached = matches!(next.frame.frame, Some(Frame::LastActivity(_)));
        sent.push(next.frame.frame);
        if reached {
            break;
        }
    }
    let last_activity_at = sent
        .iter()
        .position(|frame| matches!(frame, Some(Frame::LastActivity(_))))
        .unwrap_or_else(|| panic!("the LastActivity was never sent in 16 steps; sent {sent:?}"));
    let late_cell_at = sent
        .iter()
        .position(|frame| {
            matches!(frame, Some(Frame::CellGrid(grid))
                if grid.viewport_rows.iter().any(|row| row.spans.iter().any(|span| span.text == "LATE")))
        })
        .unwrap_or_else(|| panic!("the LATE cell was never sent before the drain ended; sent {sent:?}"));
    assert!(
        late_cell_at < last_activity_at,
        "within its own domain the aged non-cell waits behind the cell queued ahead of it; sent {sent:?}"
    );
    assert_eq!(
        roost_coord::sync_ws::domain_table::DOMAIN_SLOTS,
        8,
        "one slot per Sync domain"
    );
}
