//! The firehose adapters over the durable and machine-record domains, and the
//! queued-frame invariant of contract §12.8: a frame that has been queued and
//! drained still carries the metadata that was queued with it.
//!
//! A queued frame that lost its meta is invisible to the egress path that is
//! supposed to advance its cursor, so a multi-part snapshot stops after part
//! one and nothing reports why. The failure is silent and the symptom points
//! somewhere else, which is why this drives the whole path -- enqueue, drain,
//! acknowledge, enqueue again -- rather than reading the meta back off the
//! value that was handed in.
//!
//! The bus-coverage audit is in `sync_feed_bus_coverage.rs` and the volatile
//! half -- presence, last activity and the browser UI stream -- is in
//! `sync_feed_volatile.rs`, because those fan out under rules the durable
//! domains do not have.
//!
//! Every unwrap here is an assertion over a value the test just built: the panic
//! IS the failure, which is why `unwrap_used` is denied in product code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod sync_feed_support;

use std::collections::BTreeSet;

use roost_coord::events::bus_messages::{SessionBusMessage, WorkerRoutableSet};
use roost_coord::sync_ws::egress::FlushStep;
use roost_coord::sync_ws::feed::frames::{mcp_frame, session_message_frame};
use roost_coord::sync_ws::feed::worker_frames::{worker_presence_frame, worker_routable_frame};
use roost_coord::sync_ws::frame_meta::{FeedLane, SyncFrameMeta, frame_meta_for};
use roost_coord::sync_ws::terminal::snapshot::NoTerminalSnapshotHub;
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::__buffa::oneof::mcp_stream_message_proto::Kind as McpKind;
use roost_proto::__buffa::oneof::worker_presence_proto::Kind as PresenceKind;
use roost_proto::{McpStreamMessageProto, WorkerPresenceProto};
use roost_protocol::wire::{
    McpRelayEvent, McpRelayId, McpStreamMessage, SessionEvent, WorkerPresenceEvent,
};
use serde_json::json;

use sync_feed_support::{
    RELAY_A, SESSION_A, WORKER_A, WORKER_B, acknowledge, cell_frame, closed_message,
    hydrated_terminal, oneof_of, opened_message, session, worker, worker_registration,
};

#[test]
fn a_frame_queued_and_then_drained_still_carries_its_meta() {
    let mut session = hydrated_terminal();
    let mut hub = NoTerminalSnapshotHub;

    let opened = session_message_frame(&opened_message()).expect("an opened event is public");
    assert_eq!(opened.meta().announces, vec![SESSION_A.to_owned()]);
    assert_eq!(opened.meta().lane, FeedLane::Session);
    assert!(
        opened
            .enqueue_into(&mut session, 1_000, &mut hub)
            .is_queued(),
        "a session-lane frame this socket subscribed to must be queued -- not \
         refused, and not sent as an unsequenced control"
    );

    let FlushStep::Send(announcement) = session.take_next_sendable(1_000, &mut hub) else {
        panic!("the announcement must be the next frame out");
    };
    assert_eq!(announcement.delivery_seq, 1);
    acknowledge(&mut session, 1_000);

    // THE §12.8 ASSERTION. The cell is fenced behind the announcement the
    // queued frame asserted, so a cell that cannot go out now means the
    // announcement went out without the metadata that named the session.
    let cell = cell_frame();
    let cell_meta = frame_meta_for(
        cell.frame
            .as_ref()
            .expect("the test's cell carries a oneof"),
    );
    assert!(
        session
            .enqueue_frame(&cell, Some(&cell_meta), 1_000, &mut hub)
            .is_queued()
    );
    assert!(
        matches!(
            session.take_next_sendable(1_000, &mut hub),
            FlushStep::Send(sendable) if sendable.delivery_seq == 2
        ),
        "a cell for a session whose opened event was delivered AND acknowledged \
         is free to go: the announcement it was fenced behind left the queue \
         with the metadata that named the session"
    );
}

#[test]
fn a_private_session_event_never_becomes_a_browser_frame() {
    let message = SessionBusMessage::committed(
        SessionEvent::AgentReference {
            session_id: session(SESSION_A),
            reference: None,
            ts: 1_700_000_000_000,
            trace_id: None,
        },
        9,
    );
    let refused = session_message_frame(&message).expect_err("a private kind must be refused");
    assert!(
        refused.to_string().contains("agent_reference"),
        "the refusal must name the kind it refused, got: {refused}"
    );
}

#[test]
fn a_cell_for_a_session_that_was_closed_never_goes_out() {
    let mut session = hydrated_terminal();
    let mut hub = NoTerminalSnapshotHub;

    let opened = session_message_frame(&opened_message()).expect("an opened event is public");
    assert!(
        opened
            .enqueue_into(&mut session, 1_000, &mut hub)
            .is_queued()
    );
    let FlushStep::Send(announcement) = session.take_next_sendable(1_000, &mut hub) else {
        panic!("the announcement must go out first");
    };
    assert_eq!(announcement.delivery_seq, 1);
    acknowledge(&mut session, 1_000);

    let close = session_message_frame(&closed_message()).expect("a close is public");
    assert_eq!(close.meta().closes, vec![SESSION_A.to_owned()]);
    assert!(
        close
            .enqueue_into(&mut session, 1_000, &mut hub)
            .is_queued()
    );
    let FlushStep::Send(delivered) = session.take_next_sendable(1_000, &mut hub) else {
        panic!("the close must go out");
    };
    assert_eq!(delivered.delivery_seq, 2);
    acknowledge(&mut session, 1_000);

    let cell = cell_frame();
    let cell_meta = SyncFrameMeta::cell(SESSION_A);
    assert!(
        session
            .enqueue_frame(&cell, Some(&cell_meta), 1_000, &mut hub)
            .is_queued()
    );
    assert!(
        matches!(session.take_next_sendable(1_000, &mut hub), FlushStep::Idle),
        "a cell queued for a session whose close has been delivered describes \
         output from a session the client has just been told is gone, and must \
         never be sent"
    );
}

#[test]
fn an_mcp_relay_event_carries_its_payload_as_json_text() {
    let payload = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
    let frame = mcp_frame(&McpStreamMessage::Event(McpRelayEvent {
        relay_id: McpRelayId::try_from(RELAY_A).expect("the constant is a uuid"),
        payload: payload.clone(),
        ts: 1_700_000_000_000,
    }))
    .expect("a JSON payload always serialises");
    match oneof_of(&frame) {
        // The oneof holds a `Box` for every message arm, so the payload is
        // destructured through the box rather than through Deref sugar that
        // would hide where the allocation is.
        Frame::McpMsg(message) => match *message {
            McpStreamMessageProto {
                kind: Some(McpKind::Event(event)),
                ..
            } => {
                assert_eq!(event.relay_id, RELAY_A);
                assert_eq!(event.payload_json, payload.to_string());
            }
            other => panic!("a relay event must produce an event arm, got {other:?}"),
        },
        other => panic!("a relay event must produce an mcp arm, got {other:?}"),
    }
}

#[test]
fn a_worker_frame_carries_the_machine_record_a_heartbeat_omits() {
    let registered =
        worker_presence_frame(&worker_registration()).expect("a registration always projects");
    match oneof_of(&registered) {
        Frame::WorkerPresence(message) => match *message {
            WorkerPresenceProto {
                kind: Some(PresenceKind::Registered(record)),
                ..
            } => {
                assert_eq!(record.fp, WORKER_A);
                assert_eq!(record.os, "linux");
                assert!(
                    record.host_metrics.as_option().is_some(),
                    "a registration is the only frame that carries the machine's \
                     whole record, samples included"
                );
            }
            other => panic!("a registration must produce a registered presence, got {other:?}"),
        },
        other => panic!("a registration must produce a presence arm, got {other:?}"),
    }

    let heartbeat = worker_presence_frame(&WorkerPresenceEvent::Heartbeat {
        fp: worker(WORKER_A),
        last_seen_ms: 1_700_000_001_000,
        host_metrics: None,
        terminal_core_capacity: None,
    })
    .expect("a heartbeat always projects");
    match oneof_of(&heartbeat) {
        Frame::WorkerPresence(message) => match *message {
            WorkerPresenceProto {
                kind: Some(PresenceKind::Heartbeat(beat)),
                ..
            } => {
                assert_eq!(beat.worker_fp, WORKER_A);
                assert_eq!(beat.last_seen_ms, 1_700_000_001_000);
                assert!(
                    beat.host_metrics.as_option().is_none(),
                    "a heartbeat is a presence signal and nothing more"
                );
            }
            other => panic!("a heartbeat must produce a heartbeat presence, got {other:?}"),
        },
        other => panic!("a heartbeat must produce a presence arm, got {other:?}"),
    }

    let removal = worker_presence_frame(&WorkerPresenceEvent::Removed {
        fp: worker(WORKER_A),
    })
    .expect("a removal always projects");
    match oneof_of(&removal) {
        Frame::WorkerPresence(message) => match *message {
            WorkerPresenceProto {
                kind: Some(PresenceKind::RemovedFp(fp)),
                ..
            } => assert_eq!(fp, WORKER_A),
            other => panic!("a removal must produce a removed presence, got {other:?}"),
        },
        other => panic!("a removal must produce a presence arm, got {other:?}"),
    }
}

#[test]
fn the_routable_set_is_narrowed_to_the_machines_this_socket_may_see() {
    let visible = BTreeSet::from([worker(WORKER_A)]);
    let frame = worker_routable_frame(
        &WorkerRoutableSet {
            fps: vec![worker(WORKER_A), worker(WORKER_B)],
        },
        &visible,
    );
    match oneof_of(&frame) {
        Frame::WorkerRoutable(frame) => {
            assert_eq!(
                frame.fps,
                vec![WORKER_A.to_owned()],
                "a worker-owned socket is shown its own reachability and nothing else"
            );
            assert!(
                frame.snapshot_id.is_empty(),
                "an empty snapshot id is the live full-set replacement, as \
                 opposed to a chunked retained seed"
            );
        }
        other => panic!("the routable set must produce a routable frame, got {other:?}"),
    }
}
