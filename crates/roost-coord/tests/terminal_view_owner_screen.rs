//! An owner-mode session end to end through the view hub AND the real screen
//! replica: the owner's decision puts a socket on the stream, cells reach it
//! until its last view goes inactive, a lost baseline is repaired by the
//! owning worker, and only the decision that attaches a socket seeds it.
//!
//! Ports the screen cases of `apps/coord/tests/terminal/view/terminal-view-owner-mode.test.ts`;
//! its relay cases are `terminal_view_relay.rs`. The socket below stands in
//! for `sync_ws::terminal::screen_socket::SyncScreenSocket`: its view sink
//! reaches the replica exactly as that one does.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod terminal_screen_hub_support;
mod terminal_view_support;

use std::sync::Arc;

use roost_coord::terminal_screen::ScreenHub;
use roost_coord::terminal_screen::hub_contract::{ScreenTimers, TerminalScreenSocketSink};
use roost_coord::terminal_screen::hub_state::ScreenCheckpoint;
use roost_coord::terminal_screen::screen_budget::TerminalScreenCaps;
use roost_coord::terminal_view::{SocketRegistration, TerminalViewSink, owner_screen_repair};
use roost_proto::FirehoseFrame;
use roost_protocol::wire::SessionId;
use terminal_screen_hub_support::{ManualTimers, TestSink, full_frame};
use terminal_view_support::{
    FINGERPRINT, Harness, OTHER_VIEW, Relayed, SESSION, VIEW, owner_state,
};

const STREAM_A: &str = "5a000000-0000-4000-8000-000000000001";
const STREAM_B: &str = "5a000000-0000-4000-8000-000000000002";

/// A browser socket as both hubs reach it.
///
/// `order` records what the socket was told, IN THE ORDER IT WAS TOLD, because
/// the replica folds a cell only against the stream its last view-state named:
/// a baseline that overtakes its own state is refused as stale and, per
/// `admit_frame`, never latches a repair.
#[derive(Default)]
struct OwnerSocket {
    screens: Arc<ScreenHub>,
    order: std::sync::Mutex<Vec<&'static str>>,
}

impl TerminalViewSink for OwnerSocket {
    fn enqueue_terminal_state(&self, _: &str, _: FirehoseFrame, _: &str) {
        self.order
            .lock()
            .expect("the order log is never poisoned")
            .push("state");
    }

    fn set_watching(&self, socket_id: &str, session_id: &SessionId, watching: bool) {
        self.screens.set_watching(socket_id, session_id, watching);
    }

    fn seed_socket(&self, socket_id: &str, session_id: &SessionId) -> bool {
        let seeded = self.screens.seed_socket(socket_id, session_id);
        self.order
            .lock()
            .expect("the order log is never poisoned")
            .push("seed");
        seeded
    }

    fn resync_socket(&self, socket_id: &str, session_id: &SessionId, grid_epoch: &str, seq: u64) {
        let checkpoint = ScreenCheckpoint {
            grid_epoch: grid_epoch.to_owned(),
            seq,
        };
        self.screens
            .resync_socket(socket_id, session_id, Some(&checkpoint));
    }

    fn live_view_expired(&self, _: &str, _: &str, _: &SessionId) {}

    fn expect_stream(&self, session_id: &SessionId, stream_id: &str, cols: u32, rows: u32) {
        self.screens
            .expect_stream(session_id, stream_id, cols, rows);
    }

    fn expected_stream_id(&self, session_id: &SessionId) -> Option<String> {
        self.screens.expected_stream_id(session_id)
    }

    fn invalidate(&self, session_id: &SessionId, reason: &str) {
        self.screens.invalidate(session_id, reason);
    }
}

/// A view hub with one owner-mode worker, and the replica it repairs through.
struct Owned {
    views: Harness,
    screens: Arc<ScreenHub>,
}

impl Owned {
    fn new() -> Self {
        let views = Harness::new();
        let screens = Arc::new(ScreenHub::with_deadlines(
            TerminalScreenCaps {
                max_resident_rows: 65_536,
                max_resident_spans: 2_097_152,
            },
            owner_screen_repair(&views.hub),
            Arc::new(ManualTimers::default()) as Arc<dyn ScreenTimers>,
            Arc::new(|| 0),
        ));
        Self { views, screens }
    }

    /// Register `socket_id` with both hubs; the returned sink records cells.
    fn socket(&self, socket_id: &str) -> Arc<TestSink> {
        self.registered_socket(socket_id).0
    }

    /// The same registration, with the socket whose wire order it is.
    fn registered_socket(&self, socket_id: &str) -> (Arc<TestSink>, Arc<OwnerSocket>) {
        let cells = TestSink::queuing();
        self.screens.register_socket(
            socket_id,
            Arc::clone(&cells) as Arc<dyn TerminalScreenSocketSink>,
        );
        let socket = Arc::new(OwnerSocket {
            screens: Arc::clone(&self.screens),
            ..OwnerSocket::default()
        });
        self.views.hub.register_socket(
            &SocketRegistration {
                socket_id: socket_id.to_owned(),
                viewer_key: Some(format!("{FINGERPRINT}:{socket_id}")),
                caller_fingerprint: FINGERPRINT.to_owned(),
                session_ids: [SESSION.to_owned()].into_iter().collect(),
                sink: Arc::clone(&socket) as Arc<dyn TerminalViewSink>,
            },
            0,
        );
        (cells, socket)
    }

    /// The owner's decision for one view on `socket_id`.
    fn decide(&self, socket_id: &str, view_id: &str, revision: u64, stream_id: &str) {
        let active = !stream_id.is_empty();
        let (cols, rows) = if active { (8, 2) } else { (0, 0) };
        let frame = owner_state(view_id, SESSION, revision, active, stream_id, cols, rows);
        self.views
            .hub
            .apply_owner_view_state(&self.views.worker, socket_id, &frame);
    }

    /// The owner's decision for one view at an explicit effective geometry,
    /// which is what a second viewer narrowing the grid actually changes.
    fn decide_sized(
        &self,
        socket_id: &str,
        view_id: &str,
        revision: u64,
        stream_id: &str,
        cols: u32,
        rows: u32,
    ) {
        let active = !stream_id.is_empty();
        let (cols, rows) = if active { (cols, rows) } else { (0, 0) };
        let frame = owner_state(view_id, SESSION, revision, active, stream_id, cols, rows);
        self.views
            .hub
            .apply_owner_view_state(&self.views.worker, socket_id, &frame);
    }

    /// A worker baseline at an explicit stream and geometry.
    fn baseline_sized(&self, stream_id: &str, seq: u64, cols: u32, rows: u32) {
        let mut frame = full_frame(stream_id, seq, cols, rows, &[]);
        self.screens
            .publish_frame(&self.views.session, &mut frame, 0);
    }

    fn baseline(&self, seq: u64) {
        let mut frame = full_frame(STREAM_A, seq, 8, 2, &[]);
        self.screens
            .publish_frame(&self.views.session, &mut frame, 0);
    }

    fn snapshot_repairs(&self) -> Vec<(String, String)> {
        self.views
            .transport
            .relayed()
            .into_iter()
            .filter_map(|relayed| match relayed {
                Relayed::Snapshot {
                    session_id,
                    stream_id,
                    ..
                } => Some((session_id, stream_id)),
                _ => None,
            })
            .collect()
    }
}

fn served(sink: &TestSink) -> Vec<(String, String)> {
    sink.snapshots()
        .into_iter()
        .map(|served| (served.session_id, served.stream_id))
        .collect()
}

// v2 "cells fan out to an owner-mode socket and stop when its last view goes
// inactive".
#[test]
fn cells_reach_an_owner_mode_socket_until_its_last_view_goes_inactive() {
    let owned = Owned::new();
    let cells = owned.socket("socket-a");
    owned.decide("socket-a", VIEW, 1, STREAM_A);

    owned.baseline(1);
    assert_eq!(served(&cells), [(SESSION.to_owned(), STREAM_A.to_owned())]);

    owned.decide("socket-a", VIEW, 2, "");
    assert_eq!(cells.drops(), [SESSION]);
    owned.baseline(2);
    assert_eq!(
        cells.snapshots().len(),
        1,
        "an unwatched socket hears no further cells"
    );
}

// v2 "a replica whose baseline is lost obtains a source full from the owning
// worker".
#[test]
fn a_lost_baseline_is_repaired_by_the_owning_worker_once() {
    let owned = Owned::new();
    let cells = owned.socket("socket-a");
    owned.decide("socket-a", VIEW, 1, STREAM_A);
    owned.baseline(1);
    assert_eq!(cells.snapshots().len(), 1);
    assert!(owned.snapshot_repairs().is_empty());

    owned
        .screens
        .invalidate(&owned.views.session, "worker upstream delta loss");
    assert_eq!(
        owned.snapshot_repairs(),
        [(SESSION.to_owned(), STREAM_A.to_owned())]
    );

    owned.decide("socket-a", VIEW, 2, STREAM_A);
    assert_eq!(
        owned.snapshot_repairs().len(),
        1,
        "a view resuming the same stream stacks no second request"
    );
}

// v2 "a lease heartbeat on an attached view pushes no second baseline".
#[test]
fn a_lease_heartbeat_on_an_attached_view_pushes_no_second_baseline() {
    let owned = Owned::new();
    let cells = owned.socket("socket-a");
    owned.decide("socket-a", VIEW, 1, STREAM_A);
    owned.baseline(1);
    assert_eq!(cells.snapshots().len(), 1);

    owned.decide("socket-a", VIEW, 2, STREAM_A);
    owned.decide("socket-a", VIEW, 3, STREAM_A);
    assert_eq!(cells.snapshots().len(), 1);
}

// v2 "a socket attaching to a stream the replica already holds is seeded from
// it".
#[test]
fn a_socket_attaching_to_a_held_stream_is_seeded_from_the_replica() {
    let owned = Owned::new();
    owned.socket("socket-a");
    owned.decide("socket-a", VIEW, 1, STREAM_A);
    owned.baseline(1);
    let second = owned.socket("socket-b");

    owned.decide("socket-b", OTHER_VIEW, 1, STREAM_A);

    assert_eq!(served(&second), [(SESSION.to_owned(), STREAM_A.to_owned())]);
    assert!(
        owned.snapshot_repairs().is_empty(),
        "the replica served it; the worker was not asked"
    );
}

// v2 "a new stream id asks for no source full".
#[test]
fn a_new_stream_id_asks_the_owner_for_no_source_full() {
    let owned = Owned::new();
    owned.socket("socket-a");

    owned.decide("socket-a", VIEW, 1, STREAM_A);
    owned.decide("socket-a", VIEW, 2, STREAM_B);

    assert!(owned.snapshot_repairs().is_empty());
    assert_eq!(
        owned
            .screens
            .expected_stream_id(&owned.views.session)
            .as_deref(),
        Some(STREAM_B)
    );
}

// THE ORDER. A browser folds a cell only against the stream its LAST view-state
// named, so an attach that seeds must put the state on the wire FIRST. Seeded
// first, the baseline reaches a replica that has been told nothing, is refused
// by `admit_frame` as stale against a token it never expected, and — per that
// function's own comment — deliberately latches no repair, so the pane stays
// blank forever with no `screen_resync` to notice. The first viewer escapes it
// by luck: its `seed_socket` finds no resident cache, so nothing is seeded and
// the baseline arrives later with the worker's own full.
#[test]
fn an_attach_that_seeds_sends_the_view_state_before_the_baseline() {
    let owned = Owned::new();
    owned.socket("socket-a");
    owned.decide("socket-a", VIEW, 1, STREAM_A);
    owned.baseline(1);
    let (_, second) = owned.registered_socket("socket-b");

    owned.decide("socket-b", OTHER_VIEW, 1, STREAM_A);

    let order = second
        .order
        .lock()
        .expect("the order log is never poisoned")
        .clone();
    assert_eq!(
        order,
        ["state", "seed"],
        "a seeded attach must reach the socket state-first; a baseline ahead of \
         its own view-state is refused as stale and never latches a repair, so \
         the second viewer paints nothing forever"
    );
}

// A SECOND VIEWER NARROWING THE GRID. The incumbent's view never re-attaches:
// it holds the same view id, on the same socket, while the owner mints a NEW
// stream because the effective per-axis minimum dropped. The incumbent's
// replica is therefore dropped by `expect_stream` and it owes itself a
// baseline on the new stream, but nothing in its own path re-attaches it — so
// if the seed fan-out does not reach it, the pane keeps painting the old grid
// forever and no liveness signal ever fires, because the lane is healthy.
#[test]
fn a_second_viewer_narrowing_the_grid_seeds_the_incumbent_on_the_new_stream() {
    let owned = Owned::new();
    let (incumbent_cells, _) = owned.registered_socket("socket-a");

    // The incumbent alone, at its own 62x46.
    owned.decide_sized("socket-a", VIEW, 1, STREAM_A, 62, 46);
    owned.baseline_sized(STREAM_A, 1, 62, 46);
    assert_eq!(
        served(&incumbent_cells),
        [(SESSION.to_owned(), STREAM_A.to_owned())]
    );

    // A second viewer joins at a shorter geometry, the effective per-axis
    // minimum narrows to 62x27, and the owner mints a second stream.
    let (joiner_cells, _) = owned.registered_socket("socket-b");
    owned.decide_sized("socket-a", VIEW, 2, STREAM_B, 62, 27);
    owned.decide_sized("socket-b", OTHER_VIEW, 1, STREAM_B, 62, 27);
    owned.baseline_sized(STREAM_B, 2, 62, 27);

    assert_eq!(
        incumbent_cells.begins().last(),
        Some(&(SESSION.to_owned(), STREAM_B.to_owned())),
        "the incumbent's lane must be restarted onto the new stream"
    );
    assert_eq!(
        served(&incumbent_cells).last(),
        Some(&(SESSION.to_owned(), STREAM_B.to_owned())),
        "the stream change dropped this replica, so only the new stream's \
         baseline can repaint it; leaving the old grid up shows the pane a \
         size no viewer asked for, and no repair is owed because the lane is \
         healthy"
    );
    assert_eq!(
        served(&joiner_cells).last(),
        Some(&(SESSION.to_owned(), STREAM_B.to_owned())),
        "the joiner is served the same baseline as the incumbent"
    );
}
