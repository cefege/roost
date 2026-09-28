//! Fixtures for the Sync v2 send-queue tests: the frames they queue, the client
//! context, and the two hydrated sockets — one whose snapshot seed covers the
//! session under test and one whose seed does not.
//!
//! A module rather than part of `sync_v2_send_queue.rs` because `tests/*.rs` are
//! separate crates and the fixtures would otherwise push that file past 400.

use std::collections::BTreeSet;
use std::sync::Arc;

use roost_coord::sync_ws::commands::{ClientContext, CommandOutcome, handle_client_frame};
use roost_coord::sync_ws::domain_table::DomainGenerations;
use roost_coord::sync_ws::frame_meta::{SyncFrameMeta, frame_meta_for};
use roost_coord::sync_ws::session::SyncV2Session;
use roost_coord::sync_ws::snapshot_registry::SnapshotTokenRegistry;
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::__buffa::oneof::session_event_proto::Kind;
use roost_proto::__buffa::oneof::sync_client_frame::Command as ClientCommand;
use roost_proto::{
    FirehoseFrame, OpenedEvt, PbCellGridFrame, PbCellRow, PbCellSpan, SessionEventProto,
    SyncClientFrame, SyncDomain, SyncDomainReadyCommand,
};

pub(crate) const SESSION_A: &str = "11111111-1111-4111-8111-111111111111";
/// A session the snapshot seed does NOT cover, so hydration leaves it
/// unannounced and its cells stay fenced. See `hydrated_uncovered_terminal`.
pub(crate) const SESSION_B: &str = "22222222-2222-4222-8222-222222222222";
pub(crate) const SNAPSHOT_A: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

pub(crate) fn generations() -> Arc<DomainGenerations> {
    Arc::new(DomainGenerations::new(1_000))
}

pub(crate) fn context() -> ClientContext {
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

pub(crate) fn cell_frame(session_id: &str, marker: &str, seq: u64) -> FirehoseFrame {
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

pub(crate) fn opened_event(session_id: &str) -> FirehoseFrame {
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

pub(crate) fn meta_of(frame: &FirehoseFrame) -> SyncFrameMeta {
    frame_meta_for(frame.frame.as_ref().expect("a test frame carries a oneof"))
}

pub(crate) fn hydrated_terminal() -> (SyncV2Session, SnapshotTokenRegistry) {
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

/// A hydrated socket watching a session the snapshot seed did NOT cover.
///
/// `hydrated_terminal` binds the token over `SESSION_A`, and
/// `handle_domain_ready` seeds `announced_sessions` from the covered set exactly
/// as v2 does (`sync-ws-v2-commands.ts:127-130`) — so on that fixture the cells
/// are already unfenced when the test queues them. **The fence is correct; the
/// fixture was wrong.** Binding over a DIFFERENT session is the shape a real
/// socket is in: hydration has happened, and the session under test is one the
/// seed did not mention.
///
/// The alternative — an empty covered set — would stop exercising hydration
/// altogether, which is the state that matters.
pub(crate) fn hydrated_uncovered_terminal() -> (SyncV2Session, SnapshotTokenRegistry) {
    let mut session = SyncV2Session::new("socket-1".to_owned(), generations(), true);
    let mut tokens = SnapshotTokenRegistry::new();
    tokens.register_socket("socket-1", "fingerprint");
    let mut covered = BTreeSet::new();
    covered.insert(SESSION_B.to_owned());
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
