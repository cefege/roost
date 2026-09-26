//! What arrives on a route: the cell frames, and the two ways one is refused.
//!
//! Split from `terminal_screen_route_index.rs` because the index's job is
//! answering "which session does this channel carry" and this file's job is
//! what happens to a frame once it has been answered. A frame naming a
//! session its route does not carry is a protocol violation, and the answer is
//! to invalidate the replica rather than to relay the frame.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use roost_coord::coord_core::seams::{LiveChannel, WorkerRouteIndex};
use roost_coord::terminal_screen::byte_hub::{ByteHub, PublishOutcome};
use roost_coord::terminal_screen::route_index::{RouteRetirement, RouteRetirementSink};
use roost_protocol::wire::{ChannelId, SessionId, WorkerFp};

const WORKER_A: &str = "aa00000000000000000000000000000000000000000000000000000000000000";

/// Records every retirement, so a test can name what stopped resolving.
#[derive(Default)]
struct RecordingSink(Mutex<Vec<RouteRetirement>>);

impl RouteRetirementSink for RecordingSink {
    fn route_retired(&self, retirement: &RouteRetirement) {
        self.0
            .lock()
            .expect("the recording sink is not poisoned")
            .push(retirement.clone());
    }
}

fn worker(raw: &str) -> WorkerFp {
    WorkerFp::try_from(raw).unwrap()
}

fn session(raw: &str) -> SessionId {
    SessionId::try_from(raw).unwrap()
}

fn uuid(tail: &str) -> String {
    format!("00000000-0000-4000-8000-{tail:0>12}")
}

fn channel(raw: i64) -> ChannelId {
    ChannelId::try_from(raw).unwrap()
}

fn hub(sink: &Arc<RecordingSink>) -> ByteHub {
    ByteHub::new(
        Arc::new(roost_coord::terminal_screen::replica::ScreenHub::new()),
        Arc::clone(sink) as Arc<dyn RouteRetirementSink>,
    )
}

#[test]
fn a_frame_on_an_unbound_channel_is_dropped_and_a_sustained_burst_is_raised() {
    let hub = ByteHub::with_defaults();
    let mut frame = roost_proto::PbCellGridFrame::default();

    let mut outcomes = Vec::new();
    for step in 0..60 {
        outcomes.push(hub.publish_cell_grid(&worker(WORKER_A), channel(5), &mut frame, step * 100));
    }

    assert!(
        outcomes
            .iter()
            .all(|outcome| *outcome == PublishOutcome::DroppedUnmapped),
        "a channel nothing resolves never reaches a replica"
    );
}

#[test]
fn a_frame_whose_session_disagrees_with_its_route_is_refused_not_relayed() {
    let sink = Arc::new(RecordingSink::default());
    let hub = hub(&sink);
    let routed = session(&uuid("1"));
    let stranger = session(&uuid("2"));
    hub.replace_worker_channel_index(
        &worker(WORKER_A),
        &[LiveChannel {
            session_id: routed.clone(),
            channel_id: channel(1),
        }],
    );
    hub.screens()
        .expect_stream(&routed, "3f2504e0-4f89-41d3-9a0c-0305e82c3301", 80, 24);
    let mut frame = roost_proto::PbCellGridFrame {
        session_id: stranger.as_str().to_owned(),
        stream_id: "3f2504e0-4f89-41d3-9a0c-0305e82c3301".to_owned(),
        cols: 80,
        rows: 24,
        full: true,
        ..Default::default()
    };

    let outcome = hub.publish_cell_grid(&worker(WORKER_A), channel(1), &mut frame, 1_000);

    assert_eq!(
        outcome,
        PublishOutcome::DroppedMismatchedSession {
            session_id: routed.clone()
        }
    );
    assert!(
        !hub.screens().has_valid_cache(&routed),
        "the replica is invalidated rather than fed a frame belonging to another session"
    );
}
