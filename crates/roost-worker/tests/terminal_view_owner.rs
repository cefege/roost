//! Ports `apps/worker/tests/terminal/view/terminal-view-owner.test.ts`: the
//! worker is the authority for its own sessions' terminal views. Geometry is the
//! minimum over live viewers, a parked viewer stops constraining only when its
//! grace lapses, losing every viewer HOLDS the last geometry, a view decision
//! precedes its stream's first cell, a coordinator reconnect drops only relayed
//! sockets, and a trapped core spends exactly one re-proof desire.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod terminal_view_support;

use roost_proto::__buffa::oneof::d_terminal_view_relay::Command as RelayCommand;
use roost_proto::TerminalViewStatus;
use roost_protocol::viewport::{TERMINAL_VIEW_LEASE_MS, TERMINAL_VIEW_PARK_GRACE_MS};
use terminal_view_support::{
    Fixture, Installed, Outcome, device, remote_device, view_command, view_id,
};

fn dims(installed: &Installed) -> (u32, u32) {
    (installed.cols, installed.rows)
}

#[tokio::test]
async fn effective_geometry_is_the_minimum_over_live_viewers() {
    let mut fixture = Fixture::new(&[]);
    let wide = fixture.local_socket("socket-wide", "tab-wide");
    fixture.local_socket("socket-narrow", "tab-narrow");

    fixture
        .owner
        .handle_view_command("socket-wide", &view_command(&view_id(1), 100, 30, 1));
    fixture.settle().await;
    assert_eq!(dims(&fixture.sessions.installed()), (100, 30));

    fixture
        .owner
        .handle_view_command("socket-narrow", &view_command(&view_id(2), 80, 24, 1));
    fixture.settle().await;

    assert_eq!(dims(&fixture.sessions.installed()), (80, 24));
    let last = wide.states().pop().unwrap();
    assert_eq!(last.status.as_known(), Some(TerminalViewStatus::Accepted));
    assert_eq!((last.effective_cols, last.effective_rows), (80, 24));

    // Each axis minimizes on its own, so the answer is no single viewer's size:
    // a decider that picked one viewer, in any order, cannot produce 80x20.
    fixture.local_socket("socket-short", "tab-short");
    fixture
        .owner
        .handle_view_command("socket-short", &view_command(&view_id(3), 120, 20, 1));
    fixture.settle().await;
    assert_eq!(dims(&fixture.sessions.installed()), (80, 20));
}

#[tokio::test]
async fn a_parked_viewer_stops_constraining_only_once_its_grace_lapses() {
    let mut fixture = Fixture::new(&[]);
    fixture.local_socket("socket-wide", "tab-wide");
    fixture.local_socket("socket-narrow", "tab-narrow");
    fixture
        .owner
        .handle_view_command("socket-wide", &view_command(&view_id(1), 100, 30, 1));
    fixture
        .owner
        .handle_view_command("socket-narrow", &view_command(&view_id(2), 80, 24, 1));
    fixture.settle().await;
    let constrained = fixture.sessions.installed();

    fixture.owner.close_socket("socket-narrow");
    fixture.owner.sweep_now();
    fixture.settle().await;
    // Park absorbs reconnect wobble: inside the grace the PTY must not resize.
    assert_eq!(fixture.sessions.installed(), constrained);

    fixture.advance(TERMINAL_VIEW_PARK_GRACE_MS + 1);
    fixture.owner.sweep_now();
    fixture.settle().await;

    let widened = fixture.sessions.installed();
    assert_eq!(dims(&widened), (100, 30));
    assert_ne!(widened.stream_id, constrained.stream_id);
}

#[tokio::test]
async fn losing_every_live_viewer_holds_the_last_geometry_and_mints_no_stream() {
    let mut fixture = Fixture::new(&[]);
    fixture.local_socket("socket-wide", "tab-wide");
    fixture.local_socket("socket-narrow", "tab-narrow");
    fixture
        .owner
        .handle_view_command("socket-wide", &view_command(&view_id(1), 100, 30, 1));
    fixture
        .owner
        .handle_view_command("socket-narrow", &view_command(&view_id(2), 80, 24, 1));
    fixture.settle().await;
    let held = fixture.sessions.installed();

    fixture.owner.close_socket("socket-wide");
    fixture.owner.close_socket("socket-narrow");
    fixture.advance(TERMINAL_VIEW_PARK_GRACE_MS + 1);
    fixture.owner.sweep_now();
    fixture.settle().await;

    assert_eq!(fixture.sessions.installed(), held);
}

#[tokio::test]
async fn a_view_decision_reaches_the_socket_before_its_streams_first_cell() {
    let mut fixture = Fixture::new(&[]);
    let pane = fixture.local_socket("socket-pane", "tab-pane");

    fixture
        .owner
        .handle_view_command("socket-pane", &view_command(&view_id(1), 40, 12, 1));
    fixture.settle().await;

    let stream_id = fixture.sessions.installed().stream_id;
    let order = pane.order();
    let state_at = order
        .iter()
        .position(|entry| entry.starts_with(&format!("state:{stream_id}:")));
    let cell_at = order
        .iter()
        .position(|entry| *entry == format!("cell:{stream_id}"));
    let (Some(state_at), Some(cell_at)) = (state_at, cell_at) else {
        panic!("the socket saw neither the decision nor the cell in {order:?}");
    };
    assert!(
        cell_at > state_at,
        "a cell preceded its decision: {order:?}"
    );
    let decision = &pane.states()[state_at];
    assert_eq!(
        decision.status.as_known(),
        Some(TerminalViewStatus::Accepted)
    );
    assert_eq!(decision.stream_id, stream_id);
    assert_eq!((decision.effective_cols, decision.effective_rows), (40, 12));
}

#[tokio::test]
async fn a_coordinator_reconnect_drops_only_its_own_sockets() {
    let mut fixture = Fixture::new(&[]);
    let pane = fixture.local_socket("socket-pane", "tab-pane");
    let pane_view = view_id(1);
    fixture
        .owner
        .handle_view_command("socket-pane", &view_command(&pane_view, 100, 30, 1));
    fixture.owner.handle_relay(roost_proto::DTerminalViewRelay {
        socket_id: "remote-socket".to_owned(),
        viewer_key: format!("{}:remote-tab", remote_device()),
        device_fingerprint: remote_device(),
        budget_ms: 8_000,
        command: Some(RelayCommand::View(Box::new(view_command(
            &view_id(2),
            80,
            24,
            1,
        )))),
        ..roost_proto::DTerminalViewRelay::default()
    });
    fixture.settle().await;
    let shared = fixture.sessions.installed();
    assert_eq!(dims(&shared), (80, 24));
    assert_eq!(
        fixture.relayed.last().map(|(socket, _)| socket.as_str()),
        Some("remote-socket")
    );

    fixture.owner.drop_coordinator_sockets();
    fixture.settle().await;
    // Nothing about the live stream may move on a coordinator bounce.
    assert_eq!(fixture.sessions.installed(), shared);
    assert!(fixture.sessions.has_sink("local:socket-pane"));

    fixture.advance(TERMINAL_VIEW_PARK_GRACE_MS + 1);
    fixture.owner.sweep_now();
    fixture.settle().await;
    assert_eq!(dims(&fixture.sessions.installed()), (100, 30));
    let viewers: Vec<(String, bool, bool)> = fixture
        .projections
        .last()
        .unwrap()
        .viewers
        .iter()
        .map(|viewer| (viewer.fingerprint.clone(), viewer.parked, viewer.constrains))
        .collect();
    assert_eq!(
        viewers,
        vec![(device(), false, true), (remote_device(), true, false)]
    );

    // The local view's lease is untouched, so renewing it at the same revision
    // keeps it live past the tick that reaps the coordinator's parked record.
    fixture.advance(TERMINAL_VIEW_LEASE_MS - TERMINAL_VIEW_PARK_GRACE_MS);
    fixture
        .owner
        .handle_view_command("socket-pane", &view_command(&pane_view, 100, 30, 1));
    fixture.owner.sweep_now();
    fixture.settle().await;
    let projection = fixture.projections.last().unwrap();
    let rows: Vec<(String, u32, u32, bool)> = projection
        .viewers
        .iter()
        .map(|viewer| {
            (
                viewer.fingerprint.clone(),
                viewer.cols,
                viewer.rows,
                viewer.constrains,
            )
        })
        .collect();
    assert_eq!(rows, vec![(device(), 100, 30, true)]);
    assert_eq!(
        (projection.effective_cols, projection.effective_rows),
        (100, 30)
    );
    assert_eq!(pane.expiries.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_trapped_core_re_proves_itself_on_the_desire_the_trap_triggers() {
    let mut fixture = Fixture::new(&[Outcome::CoreFailed, Outcome::Commit]);
    let pane = fixture.local_socket("socket-pane", "tab-pane");

    fixture
        .owner
        .handle_view_command("socket-pane", &view_command(&view_id(1), 40, 12, 1));
    fixture.settle().await;

    // The trap drives exactly ONE further desire — the re-proof attempt. A
    // third generation here would mean the repair loops on its own verdict.
    let desired = pane.accepted_stream_ids();
    assert_eq!(desired.len(), 2, "{desired:?}");
    assert_ne!(desired[1], desired[0]);
    let last = pane.states().pop().unwrap();
    assert_eq!(last.status.as_known(), Some(TerminalViewStatus::Accepted));
    assert_eq!(last.stream_id, desired[1]);
    assert_eq!((last.effective_cols, last.effective_rows), (40, 12));
    let installed = fixture.sessions.installed();
    assert!(installed.core_valid);
    assert_eq!(
        (installed.stream_id.as_str(), installed.cols, installed.rows),
        (desired[1].as_str(), 40, 12)
    );
    assert!(pane.order().contains(&format!("cell:{}", desired[1])));
}

#[tokio::test]
async fn a_trap_the_keeper_cannot_re_prove_stays_fail_closed_and_desires_nothing_more() {
    let mut fixture = Fixture::new(&[Outcome::CoreFailed, Outcome::CoreFailed]);
    let pane = fixture.local_socket("socket-pane", "tab-pane");

    fixture
        .owner
        .handle_view_command("socket-pane", &view_command(&view_id(1), 40, 12, 1));
    fixture.settle().await;

    assert_eq!(pane.accepted_stream_ids().len(), 2);
    let last = pane.states().pop().unwrap();
    assert_eq!(
        last.status.as_known(),
        Some(TerminalViewStatus::Unavailable)
    );
    assert!(!fixture.sessions.installed().core_valid);
}
