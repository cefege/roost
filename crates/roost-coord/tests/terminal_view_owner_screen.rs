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
struct OwnerSocket {
    screens: Arc<ScreenHub>,
}

impl TerminalViewSink for OwnerSocket {
    fn enqueue_terminal_state(&self, _: &str, _: FirehoseFrame, _: &str) {}

    fn set_watching(&self, socket_id: &str, session_id: &SessionId, watching: bool) {
        self.screens.set_watching(socket_id, session_id, watching);
    }

    fn seed_socket(&self, socket_id: &str, session_id: &SessionId) -> bool {
        self.screens.seed_socket(socket_id, session_id)
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
        let cells = TestSink::queuing();
        self.screens.register_socket(
            socket_id,
            Arc::clone(&cells) as Arc<dyn TerminalScreenSocketSink>,
        );
        let socket = Arc::new(OwnerSocket {
            screens: Arc::clone(&self.screens),
        });
        self.views.hub.register_socket(
            &SocketRegistration {
                socket_id: socket_id.to_owned(),
                viewer_key: Some(format!("{FINGERPRINT}:{socket_id}")),
                caller_fingerprint: FINGERPRINT.to_owned(),
                session_ids: [SESSION.to_owned()].into_iter().collect(),
                sink: socket,
            },
            0,
        );
        cells
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
