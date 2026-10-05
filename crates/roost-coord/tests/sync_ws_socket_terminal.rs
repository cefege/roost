//! A real v2 Sync socket as the terminal view and title hubs reach it: a live
//! view whose lease lapses closes its socket `1013` once, and a subscriber that
//! arrives after a title changed is seeded with the retained, displayed title
//! of the sessions still open.
//!
//! Ports `apps/coord/tests/sync/sync-ws-keepalive.test.ts` "live terminal lease
//! expiry closes the owning v2 socket once", and the retained-title replay of
//! `apps/coord/src/terminal/terminal-title-hub.ts` (`getTitleSnapshot`,
//! released on `closed`) at the socket boundary.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod sync_seed_support;
mod sync_ws_socket_support;
mod ws_client_support;
mod ws_credential_support;

use std::collections::BTreeSet;

use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::__buffa::oneof::sync_client_frame::Command;
use roost_proto::{SyncDomain, SyncDomainReadyCommand, TerminalViewCommand, TerminalViewStatus};
use roost_protocol::viewport::TERMINAL_VIEW_LEASE_MS;
use sync_seed_support::{closed_message, insert_session, insert_worker};
use sync_ws_socket_support::{
    EXPECT, QUIET, SyncFixture, generation_of, next_firehose, read_subscribed, send_client_frame,
};
use tokio_tungstenite::tungstenite::Message;
use ws_client_support::{WsClient, next_frame};

const WORKER: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const SESSION: &str = "0b7c9a52-3f4e-4d6a-9b1c-2e8f7a6d5c4b";
const CLOSED_SESSION: &str = "1b7c9a52-3f4e-4d6a-9b1c-2e8f7a6d5c4b";
const VIEW_LIVE: &str = "7a000000-0000-4000-8000-000000000001";
const VIEW_DUPLICATE: &str = "7a000000-0000-4000-8000-000000000002";

fn terminal_ready(generation: u64, token: &str) -> Command {
    Command::DomainReady(Box::new(SyncDomainReadyCommand {
        domain: SyncDomain::Terminal.into(),
        generation,
        snapshot_token: Some(token.to_owned()),
        ..SyncDomainReadyCommand::default()
    }))
}

/// A browser socket whose terminal domain is open for `sessions`, and its id
/// and terminal generation.
async fn terminal_socket(
    fixture: &SyncFixture,
    seed: u8,
    sessions: &[&str],
) -> (WsClient, String, u64) {
    let (fingerprint, token) = fixture.enroll_browser(seed).await;
    let mut socket = fixture
        .dial_sync("flow=1&sync_v=2&tab=t1", &token)
        .await
        .socket();
    let subscribed = read_subscribed(&mut socket).await;
    let generation = generation_of(&subscribed, SyncDomain::Terminal);
    let admitted = sessions
        .iter()
        .map(|session| (*session).to_owned())
        .collect::<BTreeSet<_>>();
    let snapshot = fixture
        .services
        .feed
        .bind_session_snapshot(&subscribed.socket_id, &fingerprint, admitted)
        .expect("the live socket binds");
    send_client_frame(
        &mut socket,
        &subscribed.socket_id,
        None,
        Some(terminal_ready(generation, &snapshot)),
    )
    .await;
    (socket, subscribed.socket_id, generation)
}

// v2 sync-ws-keepalive.test.ts "live terminal lease expiry closes the owning
// v2 socket once": two live views of one socket lapse in one sweep; the socket
// is closed 1013 with the lease reason, and nothing follows the close.
#[tokio::test]
async fn a_lapsed_live_view_lease_closes_its_socket_once() {
    let fixture = SyncFixture::start("lease-expiry").await;
    insert_worker(&fixture, WORKER).await;
    insert_session(&fixture, SESSION, WORKER).await;
    let (mut socket, socket_id, generation) = terminal_socket(&fixture, 81, &[SESSION]).await;

    for view_id in [VIEW_LIVE, VIEW_DUPLICATE] {
        let command = TerminalViewCommand {
            view_id: view_id.to_owned(),
            session_id: SESSION.to_owned(),
            cols: 80,
            rows: 24,
            revision: 1,
            active: true,
            domain_generation: generation,
            ..TerminalViewCommand::default()
        };
        send_client_frame(
            &mut socket,
            &socket_id,
            None,
            Some(Command::TerminalView(Box::new(command))),
        )
        .await;
    }
    let mut accepted = BTreeSet::new();
    while accepted.len() < 2 {
        let frame = next_firehose(&mut socket, EXPECT)
            .await
            .expect("each view is answered");
        if let Some(Frame::TerminalViewState(state)) = frame.frame
            && state.status.as_known() == Some(TerminalViewStatus::Accepted)
        {
            accepted.insert(state.view_id.clone());
        }
    }

    let lapsed_at =
        u64::try_from(roost_coord::serve::now_ms()).unwrap() + 2 * TERMINAL_VIEW_LEASE_MS;
    fixture.services.views.sweep(lapsed_at);
    fixture.services.views.sweep(lapsed_at + 1);

    let close = loop {
        match next_frame(&mut socket, EXPECT)
            .await
            .expect("the socket is closed")
        {
            Message::Close(frame) => {
                break frame.map(|frame| (u16::from(frame.code), frame.reason.to_string()));
            }
            _ => continue,
        }
    };
    assert_eq!(
        close,
        Some((1013, "terminal view lease expired".to_owned()))
    );
    assert!(
        next_frame(&mut socket, QUIET).await.is_none(),
        "one close, and nothing after it"
    );
}

// v2 terminal-title-hub.ts `getTitleSnapshot` ("replayed to each new Sync
// subscriber so a fresh page load reflects the live title immediately") and
// `startTerminalTitleHub` (a closed session's title is released).
#[tokio::test]
async fn a_late_subscriber_is_seeded_with_the_retained_title_of_open_sessions_only() {
    let fixture = SyncFixture::start("late-title").await;
    insert_worker(&fixture, WORKER).await;
    insert_session(&fixture, SESSION, WORKER).await;
    insert_session(&fixture, CLOSED_SESSION, WORKER).await;
    let services = &fixture.services;
    let _release = services.titles.subscribe_session_close(&services.buses);
    services
        .titles
        .observe_title(&services.buses, SESSION, "π ⠋ build");
    services
        .titles
        .observe_title(&services.buses, SESSION, "π ⠙ build");
    services
        .titles
        .observe_title(&services.buses, CLOSED_SESSION, "gone");
    services
        .buses
        .session_bus
        .publish(closed_message(CLOSED_SESSION, 1));

    let (mut socket, _, _) = terminal_socket(&fixture, 82, &[SESSION, CLOSED_SESSION]).await;

    let frame = next_firehose(&mut socket, EXPECT)
        .await
        .expect("the retained title");
    let Some(Frame::TerminalTitle(title)) = frame.frame else {
        panic!("expected the retained title, got {:?}", frame.frame);
    };
    assert_eq!(
        (title.session_id.as_str(), title.title.as_str()),
        (SESSION, "π ⠋ build"),
        "the displayed title, not the spinner frame that was deduplicated away"
    );
    assert!(
        next_firehose(&mut socket, QUIET).await.is_none(),
        "the closed session's title was released"
    );
}

// One flush turn's frames go out with a single flush, and still arrive as
// separate binary messages in the order they were queued.
#[tokio::test]
async fn a_flush_turn_of_three_frames_arrives_as_three_messages_in_queue_order() {
    const SESSIONS: [&str; 3] = [
        "2b7c9a52-3f4e-4d6a-9b1c-2e8f7a6d5c4b",
        "3b7c9a52-3f4e-4d6a-9b1c-2e8f7a6d5c4b",
        "4b7c9a52-3f4e-4d6a-9b1c-2e8f7a6d5c4b",
    ];
    let fixture = SyncFixture::start("batch-order").await;
    insert_worker(&fixture, WORKER).await;
    for session in SESSIONS {
        insert_session(&fixture, session, WORKER).await;
    }
    let (mut socket, _, _) = terminal_socket(&fixture, 83, &SESSIONS).await;
    assert!(
        next_firehose(&mut socket, QUIET).await.is_none(),
        "nothing is retained before the titles change"
    );

    let services = &fixture.services;
    let queued = [
        (SESSIONS[2], "third-first"),
        (SESSIONS[0], "first-second"),
        (SESSIONS[1], "second-third"),
    ];
    for (session, title) in queued {
        services
            .titles
            .observe_title(&services.buses, session, title);
    }

    for (session, title) in queued {
        let frame = next_firehose(&mut socket, EXPECT)
            .await
            .expect("one message per queued frame");
        let Some(Frame::TerminalTitle(update)) = frame.frame else {
            panic!("expected a title, got {:?}", frame.frame);
        };
        assert_eq!(
            (update.session_id.as_str(), update.title.as_str()),
            (session, title)
        );
    }
}
