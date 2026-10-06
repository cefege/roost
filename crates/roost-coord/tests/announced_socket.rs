//! The announced-channel barrier on a real worker socket: a new channel's
//! first frames wait for the durable `opened` that routes them and then
//! publish in arrival order, and a barrier drop invalidates the session's
//! screen replica.
//!
//! Ports `apps/coord/tests/announce-barrier-handler.test.ts` ("a respawn's
//! binary frames publish after the commit in arrival order" over cell frames,
//! since raw PTY binary has no destination here; "semantic metadata preceding
//! a respawn waits for the exact route binding"), the terminal fast path of
//! `apps/coord/src/workers/worker-ws-handler.ts:306-338`, and the drop wiring of
//! `apps/coord/src/workers/worker-ws-upgrade.ts:21-28`. The append is parked
//! by holding the coordinator's one pooled database connection.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;
mod terminal_screen_hub_support;
mod worker_link_wire_support;
mod ws_client_support;
mod ws_credential_support;

use std::time::Duration;

use roost_proto::buffa::MessageField;
use roost_proto::{PbCellGridFrame, WCellGrid};
use roost_protocol::wire::coord_worker::{
    CoordWorkerDownstream, CoordWorkerUpstream, EventAck, TerminalMetadata,
};
use roost_protocol::wire::{ChannelId, SessionEvent, SessionId, SessionKind};
use terminal_screen_hub_support::{STREAM, delta_frame, full_frame};
use worker_link_wire_support::{WireFixture, next_downstream, upstream_bytes};
use ws_client_support::{WsClient, send_binary};

const SESSION: &str = "33333333-3333-4333-8333-333333333333";
const OTHER_SESSION: &str = "33333333-3333-4333-8333-333333333334";

fn session() -> SessionId {
    SessionId::try_from(SESSION).unwrap()
}

fn channel(id: u32) -> ChannelId {
    ChannelId::try_from(i64::from(id)).unwrap()
}

fn event(event: SessionEvent, client_seq: u64) -> Vec<u8> {
    upstream_bytes(&CoordWorkerUpstream::Event {
        event,
        client_seq,
        trace_id: None,
    })
}

fn snapshot(fixture: &WireFixture) -> Vec<u8> {
    event(
        SessionEvent::Snapshot {
            worker_fp: fixture.fp(),
            sessions: Vec::new(),
            ts: 1_500,
            trace_id: None,
        },
        1,
    )
}

fn opened(fixture: &WireFixture, session_id: &str, on: u32, client_seq: u64) -> Vec<u8> {
    let opened = SessionEvent::Opened {
        session_id: SessionId::try_from(session_id).unwrap(),
        worker_fp: fixture.fp(),
        channel: channel(on),
        session_kind: SessionKind::Shell,
        cwd: "/tmp".to_owned(),
        ts: 2_000,
        trace_id: None,
    };
    event(opened, client_seq)
}

fn cell(on: u32, mut frame: PbCellGridFrame) -> Vec<u8> {
    // The route stamps the session; a worker frame names none of its own.
    frame.session_id = String::new();
    upstream_bytes(&CoordWorkerUpstream::CellGrid(WCellGrid {
        channel_id: on,
        frame: MessageField::some(frame),
        ..Default::default()
    }))
}

fn title(on: u32, text: &str) -> Vec<u8> {
    upstream_bytes(&CoordWorkerUpstream::TerminalMetadata(TerminalMetadata {
        channel_id: channel(on),
        title_changed: true,
        title: text.to_owned(),
        activity_changed: false,
        activity_ts_ms: 0,
    }))
}

/// The next frame that is not a keepalive ping.
async fn next_non_ping(socket: &mut WsClient) -> CoordWorkerDownstream {
    loop {
        match next_downstream(socket).await.expect("a coordinator frame") {
            CoordWorkerDownstream::Ping { .. } => {}
            frame => return frame,
        }
    }
}

async fn expect_ack(socket: &mut WsClient, client_seq: u64) {
    assert_eq!(
        next_non_ping(socket).await,
        CoordWorkerDownstream::EventAck(EventAck { client_seq })
    );
}

/// A ready link: hello, then the snapshot that crosses the barrier.
async fn ready_link(fixture: &WireFixture) -> WsClient {
    let (mut socket, _ack) = fixture.hello_link().await;
    send_binary(&mut socket, snapshot(fixture)).await;
    expect_ack(&mut socket, 1).await;
    socket
}

/// Poll `probe` until it holds or two seconds pass.
async fn eventually(mut probe: impl FnMut() -> bool) -> bool {
    for _ in 0..200 {
        if probe() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    probe()
}

fn published_title(fixture: &WireFixture) -> Option<String> {
    let titles = fixture.services.titles.title_snapshot();
    let entry = titles
        .into_iter()
        .find(|entry| entry.session_id == SESSION)?;
    Some(entry.title)
}

#[tokio::test]
async fn a_new_channels_first_frames_wait_for_its_durable_route_and_publish_in_order() {
    let fixture = WireFixture::start("announced-order").await;
    let mut socket = ready_link(&fixture).await;
    let held = db_support::hold_every_connection(&fixture.services.db).await;

    // The title precedes the `opened` that routes its channel; the cells land
    // while that `opened` is still being appended.
    send_binary(&mut socket, title(7, "fresh-title")).await;
    send_binary(&mut socket, opened(&fixture, SESSION, 7, 2)).await;
    send_binary(&mut socket, cell(7, full_frame(STREAM, 10, 8, 2, &[]))).await;
    send_binary(&mut socket, cell(7, delta_frame(STREAM, 10, 1, "next"))).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        fixture.services.byte_hub.last_cell(&session()),
        None,
        "nothing publishes before the durable route commits"
    );
    assert_eq!(published_title(&fixture), None);

    drop(held);
    expect_ack(&mut socket, 2).await;
    let in_order = eventually(|| {
        fixture
            .services
            .byte_hub
            .last_cell(&session())
            .is_some_and(|last| last.seq == 11 && !last.full)
    })
    .await;
    assert!(in_order, "the full then the delta published, in that order");
    assert_eq!(published_title(&fixture).as_deref(), Some("fresh-title"));
}

#[tokio::test]
async fn a_barrier_drop_invalidates_the_sessions_screen_replica() {
    let fixture = WireFixture::start("announced-drop").await;
    let mut socket = ready_link(&fixture).await;
    send_binary(&mut socket, opened(&fixture, SESSION, 5, 2)).await;
    expect_ack(&mut socket, 2).await;
    let screens = fixture.services.byte_hub.screens();
    screens.expect_stream(&session(), STREAM, 8, 2);
    send_binary(&mut socket, cell(5, full_frame(STREAM, 1, 8, 2, &[]))).await;
    assert!(eventually(|| screens.has_valid_cache(&session())).await);

    let held = db_support::hold_every_connection(&fixture.services.db).await;
    let respawned = SessionEvent::Respawned {
        session_id: session(),
        new_channel: channel(9),
        ts: 3_000,
        trace_id: None,
    };
    send_binary(&mut socket, event(respawned, 3)).await;
    // A delta before any full on the respawned channel is a gap: the barrier
    // drops the channel and the replica must stop serving its old baseline.
    send_binary(&mut socket, cell(9, delta_frame(STREAM, 1, 0, "gap"))).await;

    let invalidated = eventually(|| !screens.has_valid_cache(&session())).await;
    assert!(invalidated, "the drop invalidated the session's replica");
    drop(held);
    expect_ack(&mut socket, 3).await;
}

#[tokio::test]
async fn a_routed_channels_cells_overtake_an_append_still_in_flight() {
    // v2 `message`: "terminal frames do not require a DB write" — only an
    // announced channel waits; every other channel publishes past the append.
    let fixture = WireFixture::start("announced-fast-path").await;
    let mut socket = ready_link(&fixture).await;
    send_binary(&mut socket, opened(&fixture, SESSION, 5, 2)).await;
    expect_ack(&mut socket, 2).await;

    let held = db_support::hold_every_connection(&fixture.services.db).await;
    send_binary(&mut socket, opened(&fixture, OTHER_SESSION, 7, 3)).await;
    send_binary(&mut socket, cell(5, full_frame(STREAM, 4, 8, 2, &[]))).await;

    let published = eventually(|| {
        let last = fixture.services.byte_hub.last_cell(&session());
        last.is_some_and(|last| last.seq == 4)
    })
    .await;
    assert!(
        published,
        "the routed channel's cell did not wait for the append"
    );
    drop(held);
    expect_ack(&mut socket, 3).await;
}
