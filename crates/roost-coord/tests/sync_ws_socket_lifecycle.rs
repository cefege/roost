//! The Sync socket's v2 commands end to end: the one-time snapshot token that
//! opens the terminal domain, the lazy audit domain, and the definite answer a
//! terminal command always gets.
//!
//! Ports `apps/coord/tests/sync/sync-ws-v2-snapshot-lease.test.ts`,
//! `sync-audit-subscription.test.ts` and `sync-ws-v2-terminal-command-gate.test.ts`
//! at the socket boundary. The fences at open are `sync_ws_socket_fences.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod sync_ws_socket_support;
mod ws_client_support;
mod ws_credential_support;

use std::collections::BTreeSet;

use roost_coord::events::bus_messages::{AuditRow, SessionBusMessage, SessionTitleUpdate};
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::__buffa::oneof::sync_client_frame::Command;
use roost_proto::{
    InputCommand, SyncDomain, SyncDomainReadyCommand, SyncDomainSubscriptionCommand,
};
use roost_protocol::wire::{ChannelId, SessionEvent, SessionId, SessionKind, WorkerFp};

use sync_ws_socket_support::{
    EXPECT, QUIET, SyncFixture, domain_ready, generation_of, next_firehose, read_subscribed,
    send_client_frame,
};

const SESSION: &str = "0b7c9a52-3f4e-4d6a-9b1c-2e8f7a6d5c4b";
const WORKER: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

fn opened_event() -> SessionBusMessage {
    SessionBusMessage::committed(
        SessionEvent::Opened {
            session_id: SessionId::try_from(SESSION).unwrap(),
            worker_fp: WorkerFp::try_from(WORKER).unwrap(),
            channel: ChannelId::try_from(0_i64).unwrap(),
            session_kind: SessionKind::Shell,
            cwd: "/tmp".to_owned(),
            ts: 1,
            trace_id: None,
        },
        1,
    )
}

fn terminal_ready(generation: u64, token: &str) -> Command {
    Command::DomainReady(Box::new(SyncDomainReadyCommand {
        domain: SyncDomain::Terminal.into(),
        generation,
        snapshot_token: Some(token.to_owned()),
        ..SyncDomainReadyCommand::default()
    }))
}

// v2 sync-ws-v2-snapshot-lease.test.ts: the token `SessionsList` binds to the
// live socket opens the terminal domain once; the held session frames then
// flow in order. A token bound for another key, or never bound, resets the
// domain instead (`snapshot_token_invalid`).
#[tokio::test]
async fn a_bound_snapshot_token_opens_the_terminal_domain_once() {
    let fixture = SyncFixture::start("terminal-token").await;
    let (fingerprint, token) = fixture.enroll_browser(21).await;
    let mut socket = fixture
        .dial_sync("flow=1&sync_v=2&tab=t1", &token)
        .await
        .socket();
    let subscribed = read_subscribed(&mut socket).await;
    let generation = generation_of(&subscribed, SyncDomain::Terminal);
    let feed = &fixture.services.feed;
    let sessions = BTreeSet::from([SESSION.to_owned()]);
    assert_eq!(
        feed.bind_session_snapshot(&subscribed.socket_id, "other", sessions.clone()),
        None
    );
    assert_eq!(
        feed.bind_session_snapshot("gone", &fingerprint, sessions.clone()),
        None
    );
    let snapshot = feed
        .bind_session_snapshot(&subscribed.socket_id, &fingerprint, sessions)
        .expect("the live socket binds");

    fixture.services.buses.session_bus.publish(opened_event());
    assert!(
        next_firehose(&mut socket, QUIET).await.is_none(),
        "terminal is fenced"
    );
    send_client_frame(
        &mut socket,
        &subscribed.socket_id,
        None,
        Some(terminal_ready(generation, &snapshot)),
    )
    .await;
    let opened = next_firehose(&mut socket, EXPECT)
        .await
        .expect("the held session event");
    assert!(matches!(opened.frame, Some(Frame::SessionEvent(_))));
    assert_eq!(
        (opened.delivery_seq, opened.domain.as_known()),
        (1, Some(SyncDomain::Terminal))
    );
    fixture
        .services
        .buses
        .title_bus
        .publish(SessionTitleUpdate {
            session_id: SESSION.to_owned(),
            title: "vim".to_owned(),
        });
    let title = next_firehose(&mut socket, EXPECT)
        .await
        .expect("a live title");
    assert!(matches!(title.frame, Some(Frame::TerminalTitle(_))));
    assert_eq!(title.delivery_seq, 2);

    let mut stranger = fixture
        .dial_sync("flow=1&sync_v=2&tab=t2", &token)
        .await
        .socket();
    let barrier = read_subscribed(&mut stranger).await;
    let stale = generation_of(&barrier, SyncDomain::Terminal);
    send_client_frame(
        &mut stranger,
        &barrier.socket_id,
        None,
        Some(terminal_ready(stale, &snapshot)),
    )
    .await;
    let reset = next_firehose(&mut stranger, EXPECT)
        .await
        .expect("a domain reset");
    let Some(Frame::DomainReset(reset)) = reset.frame else {
        panic!("expected a domain reset");
    };
    assert_eq!(reset.reason, "snapshot_token_invalid");
    assert_ne!(reset.generation, stale);
}

fn audit_command(subscribe: bool, generation: u64) -> Command {
    let command = Box::new(SyncDomainSubscriptionCommand {
        domain: SyncDomain::Audit.into(),
        generation,
        ..SyncDomainSubscriptionCommand::default()
    });
    if subscribe {
        Command::DomainSubscribe(command)
    } else {
        Command::DomainUnsubscribe(command)
    }
}

fn audit_row(id: i64) -> AuditRow {
    AuditRow {
        id,
        ts: 1,
        caller_fp: None,
        caller_label: None,
        method: "Ping".to_owned(),
        path: "/ping".to_owned(),
        status: 200,
        trace_id: None,
    }
}

// v2 sync-audit-subscription.test.ts "Sync v2 subscribes to audit only on
// command and holds live rows through the snapshot barrier", and the
// unsubscribe's `unsubscribed` reset.
#[tokio::test]
async fn audit_rows_flow_only_between_subscribe_ready_and_unsubscribe() {
    let fixture = SyncFixture::start("audit").await;
    let (_fp, token) = fixture.enroll_browser(22).await;
    let mut socket = fixture.dial_sync("flow=1&sync_v=2", &token).await.socket();
    let subscribed = read_subscribed(&mut socket).await;
    let generation = generation_of(&subscribed, SyncDomain::Audit);
    let socket_id = subscribed.socket_id.clone();
    let audit_bus = &fixture.services.buses.audit_bus;

    audit_bus.publish(audit_row(1));
    send_client_frame(
        &mut socket,
        &socket_id,
        None,
        Some(audit_command(true, generation)),
    )
    .await;
    tokio::time::sleep(QUIET).await;
    audit_bus.publish(audit_row(2));
    assert!(
        next_firehose(&mut socket, QUIET).await.is_none(),
        "audit is not ready"
    );
    send_client_frame(
        &mut socket,
        &socket_id,
        None,
        Some(domain_ready(SyncDomain::Audit, generation)),
    )
    .await;
    let row = next_firehose(&mut socket, EXPECT)
        .await
        .expect("the held audit row");
    let Some(Frame::AuditRow(row)) = row.frame else {
        panic!("expected an audit row");
    };
    assert_eq!(
        row.id, 2,
        "a row published before the subscription is not delivered"
    );

    send_client_frame(
        &mut socket,
        &socket_id,
        Some(1),
        Some(audit_command(false, generation)),
    )
    .await;
    let reset = next_firehose(&mut socket, EXPECT)
        .await
        .expect("the unsubscribe reset");
    let Some(Frame::DomainReset(reset)) = reset.frame else {
        panic!("expected a domain reset");
    };
    assert_eq!(
        (reset.reason.as_str(), reset.subscribed),
        ("unsubscribed", false)
    );
    audit_bus.publish(audit_row(3));
    assert!(next_firehose(&mut socket, QUIET).await.is_none());
}

// v2 sync-ws-v2-terminal-command-gate.test.ts "an input command for a
// resubscribing terminal domain is rejected, not dropped" (FAILURE-INDEX, "A
// terminal domain reset is treated as the input fence"): a terminal `input` is never
// silently dropped -- before the terminal domain is ready and after it, the
// client gets an `input_rejected` echoing its own session, sequence and
// generation.
#[tokio::test]
async fn a_terminal_input_always_gets_a_definite_answer() {
    let fixture = SyncFixture::start("input").await;
    let (fingerprint, token) = fixture.enroll_browser(23).await;
    let mut socket = fixture
        .dial_sync("flow=1&sync_v=2&tab=t1", &token)
        .await
        .socket();
    let subscribed = read_subscribed(&mut socket).await;
    let generation = generation_of(&subscribed, SyncDomain::Terminal);
    let input = |input_seq: u64| {
        Command::Input(Box::new(InputCommand {
            session_id: SESSION.to_owned(),
            input_seq,
            domain_generation: generation,
            data: b"ls\n".to_vec(),
            ..InputCommand::default()
        }))
    };
    let snapshot = fixture
        .services
        .feed
        .bind_session_snapshot(&subscribed.socket_id, &fingerprint, BTreeSet::new())
        .expect("the live socket binds");
    // Once before the terminal domain is ready, once after.
    send_client_frame(&mut socket, &subscribed.socket_id, None, Some(input(7))).await;
    send_client_frame(
        &mut socket,
        &subscribed.socket_id,
        None,
        Some(terminal_ready(generation, &snapshot)),
    )
    .await;
    send_client_frame(&mut socket, &subscribed.socket_id, None, Some(input(8))).await;
    for input_seq in [7, 8] {
        let answer = next_firehose(&mut socket, EXPECT).await.expect("an answer");
        assert_eq!(answer.delivery_seq, 0, "an answer is a control");
        let Some(Frame::InputRejected(rejected)) = answer.frame else {
            panic!("expected input_rejected");
        };
        assert_eq!(
            (
                rejected.session_id.as_str(),
                rejected.input_seq,
                rejected.domain_generation
            ),
            (SESSION, input_seq, generation)
        );
    }
}
