//! The coordinator-relay round trip through `link_ports::TerminalViewPort`: a
//! relayed view command comes back as a `terminal-view-state` for that browser
//! socket plus a `terminal-view-projection` on the real `Uplink` receiver; only
//! a local socket is seeded with a worker full; a resync is served only on the
//! current stream; a closed relayed socket parks until its grace lapses. v2:
//! `terminal-view-owner{,-screen}.ts` `handleRelay`, `seedSocket`, `closeSocket`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod terminal_view_support;

use std::sync::Arc;

use roost_proto::__buffa::oneof::d_terminal_view_relay::Command as RelayCommand;
use roost_proto::{
    DTerminalViewRelay, TerminalResyncCommand, TerminalViewCommand, TerminalViewStatus,
};
use roost_protocol::viewport::TERMINAL_VIEW_PARK_GRACE_MS;
use roost_worker::link_ports::TerminalViewPort;
use terminal_view_support::{Fixture, SESSION, remote_device, view_command, view_id};

const SOCKET: &str = "remote-socket";

fn relay(command: RelayCommand) -> DTerminalViewRelay {
    relay_from(SOCKET, command)
}

fn relay_from(socket_id: &str, command: RelayCommand) -> DTerminalViewRelay {
    DTerminalViewRelay {
        socket_id: socket_id.to_owned(),
        viewer_key: format!("{}:{socket_id}-tab", remote_device()),
        device_fingerprint: remote_device(),
        budget_ms: 8_000,
        command: Some(command),
        ..DTerminalViewRelay::default()
    }
}

fn resync(stream_id: &str) -> RelayCommand {
    RelayCommand::Resync(Box::new(TerminalResyncCommand {
        view_id: view_id(9),
        session_id: SESSION.to_owned(),
        stream_id: stream_id.to_owned(),
        grid_epoch: "epoch-1".to_owned(),
        seq: 3,
        ..TerminalResyncCommand::default()
    }))
}

#[tokio::test]
async fn a_relayed_view_answers_its_socket_and_publishes_the_projection_upstream() {
    let mut fixture = Fixture::new(&[]);
    let port: Arc<dyn TerminalViewPort> = fixture.owner.clone();

    port.relay(relay(RelayCommand::View(Box::new(view_command(
        &view_id(9),
        90,
        20,
        1,
    )))));
    fixture.settle().await;

    let installed = fixture.sessions.installed();
    assert_eq!((installed.cols, installed.rows), (90, 20));
    let (socket_id, state) = fixture.relayed.last().unwrap();
    assert_eq!(socket_id, SOCKET);
    assert_eq!(state.status.as_known(), Some(TerminalViewStatus::Accepted));
    assert_eq!(state.stream_id, installed.stream_id);
    assert_eq!((state.effective_cols, state.effective_rows), (90, 20));
    let projection = fixture.projections.last().unwrap();
    assert_eq!(projection.session_id, SESSION);
    assert_eq!(projection.stream_id, installed.stream_id);
    assert_eq!(
        (projection.effective_cols, projection.effective_rows),
        (90, 20)
    );
    let viewers: Vec<(String, u32, u32, bool, bool)> = projection
        .viewers
        .iter()
        .map(|viewer| {
            (
                viewer.fingerprint.clone(),
                viewer.cols,
                viewer.rows,
                viewer.parked,
                viewer.constrains,
            )
        })
        .collect();
    assert_eq!(viewers, vec![(remote_device(), 90, 20, false, true)]);
    // A relayed socket is seeded from the coordinator's own replica: the
    // worker requests no full for it.
    assert!(fixture.sessions.snapshots().is_empty());
}

#[tokio::test]
async fn a_joining_viewer_is_seeded_only_when_it_owns_its_sink() {
    let mut fixture = Fixture::new(&[]);
    let port: Arc<dyn TerminalViewPort> = fixture.owner.clone();
    port.relay(relay(RelayCommand::View(Box::new(view_command(
        &view_id(9),
        90,
        20,
        1,
    )))));
    fixture.settle().await;
    let stream_id = fixture.sessions.installed().stream_id;

    // A wider relayed viewer leaves the geometry alone, so it is answered on
    // the current stream — and seeded by the coordinator's replica, never by a
    // full on the one `coord` sink every remote viewer shares.
    port.relay(relay_from(
        "wider-socket",
        RelayCommand::View(Box::new(view_command(&view_id(10), 120, 40, 1))),
    ));
    fixture.settle().await;
    let (socket_id, state) = fixture.relayed.last().unwrap();
    assert_eq!(socket_id, "wider-socket");
    assert_eq!(state.status.as_known(), Some(TerminalViewStatus::Accepted));
    assert_eq!(state.stream_id, stream_id);
    assert_eq!((state.effective_cols, state.effective_rows), (90, 20));
    assert!(fixture.sessions.snapshots().is_empty());

    // A local socket owns a dedicated sink, so its seed is its own full.
    let local = fixture.local_socket("local-socket", "local-tab");
    fixture
        .owner
        .handle_view_command("local-socket", &view_command(&view_id(11), 120, 40, 1));
    fixture.settle().await;
    assert_eq!(fixture.sessions.snapshots(), vec![stream_id.clone()]);
    assert!(local.order().contains(&format!("cell:{stream_id}")));
}

#[tokio::test]
async fn a_refused_relay_is_answered_on_its_socket_with_no_stream() {
    let mut fixture = Fixture::new(&[]);
    let port: Arc<dyn TerminalViewPort> = fixture.owner.clone();
    let invalid = TerminalViewCommand {
        session_id: "not-a-session".to_owned(),
        ..view_command(&view_id(9), 90, 20, 1)
    };

    port.relay(relay(RelayCommand::View(Box::new(invalid))));
    fixture.settle().await;

    let (socket_id, state) = fixture.relayed.last().unwrap();
    assert_eq!(socket_id, SOCKET);
    assert_eq!(state.status.as_known(), Some(TerminalViewStatus::Rejected));
    assert!(state.stream_id.is_empty());
    assert!(!state.reason.is_empty());
}

#[tokio::test]
async fn a_relayed_resync_is_served_only_on_the_current_stream() {
    let mut fixture = Fixture::new(&[]);
    let port: Arc<dyn TerminalViewPort> = fixture.owner.clone();
    port.relay(relay(RelayCommand::View(Box::new(view_command(
        &view_id(9),
        90,
        20,
        1,
    )))));
    fixture.settle().await;
    let current = fixture.sessions.installed().stream_id;

    port.relay(relay(resync("00000000-0000-4000-8000-00000000dead")));
    fixture.settle().await;
    assert!(fixture.sessions.snapshots().is_empty());

    port.relay(relay(resync(&current)));
    fixture.settle().await;
    assert_eq!(fixture.sessions.snapshots(), vec![current]);
}

#[tokio::test]
async fn a_closed_relayed_socket_parks_until_its_grace_lapses_and_holds_the_geometry() {
    let mut fixture = Fixture::new(&[]);
    let port: Arc<dyn TerminalViewPort> = fixture.owner.clone();
    port.relay(relay(RelayCommand::View(Box::new(view_command(
        &view_id(9),
        90,
        20,
        1,
    )))));
    fixture.settle().await;
    let held = fixture.sessions.installed();

    port.close_socket(SOCKET);
    fixture.advance(TERMINAL_VIEW_PARK_GRACE_MS + 1);
    fixture.owner.sweep_now();
    fixture.settle().await;

    assert_eq!(fixture.sessions.installed(), held);
    let projection = fixture.projections.last().unwrap();
    let viewers: Vec<(bool, bool)> = projection
        .viewers
        .iter()
        .map(|viewer| (viewer.parked, viewer.constrains))
        .collect();
    assert_eq!(viewers, vec![(true, false)]);
    assert_eq!(
        (projection.effective_cols, projection.effective_rows),
        (90, 20)
    );
    assert_eq!(projection.stream_id, held.stream_id);
}
