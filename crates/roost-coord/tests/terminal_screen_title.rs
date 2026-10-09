//! Retained terminal titles: normalized and capped before fan-out, published
//! only on a meaningful change, deduplicated across spinner animation, and
//! retained as displayed; and the negotiated worker metadata frame that feeds
//! them, next to the raw legacy frame that must not.
//!
//! Ports `apps/coord/tests/terminal/terminal-title-hub.test.ts` and the
//! negotiated cases of `terminal-metadata-adapter.test.ts`. Its two legacy
//! parser cases have no port: the Rust coordinator carries no raw title parser
//! and refuses a legacy `Binary` metadata frame (`live_frames.rs`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;
mod frame_dispatch_support;
mod workers_support;

use std::sync::{Arc, Mutex};

use frame_dispatch_support::{LinkFixture, WORKER_FP, live_frame, worker};
use roost_coord::coord_core::worker_handle::WorkerHandle;
use roost_coord::events::bus::Subscription;
use roost_coord::events::bus_domains::Buses;
use roost_coord::events::bus_messages::SessionTitleUpdate;
use roost_coord::terminal_screen::title_hub::TerminalTitleHub;
use roost_coord::worker_link::dispatch::{DispatchOutcome, FrameDispatch};
use roost_coord::worker_link::frame_dispatch::WorkerFrameDispatcher;
use roost_protocol::versioning::CAPABILITY_TERMINAL_METADATA_V1;
use roost_protocol::wire::coord_worker::{Binary, CoordWorkerUpstream, TerminalMetadata};
use roost_protocol::wire::{ChannelId, SessionId};

const SEMANTIC_SESSION: &str = "00000000-0000-4000-8000-000000000071";
const CHANNEL: i64 = 71;

/// Every title published for `session_id` while the returned guard lives.
fn collect(
    buses: &Buses,
    session_id: &str,
) -> (Arc<Mutex<Vec<String>>>, Subscription<SessionTitleUpdate>) {
    let got = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&got);
    let wanted = session_id.to_owned();
    let subscription = buses.title_bus.subscribe(move |message| {
        if message.session_id == wanted {
            sink.lock().unwrap().push(message.title.clone());
        }
    });
    (got, subscription)
}

fn observed(session_id: &str, raw_titles: &[&str]) -> (Vec<String>, TerminalTitleHub) {
    let buses = Buses::shared();
    let hub = TerminalTitleHub::new();
    let (got, subscription) = collect(&buses, session_id);
    for raw in raw_titles {
        hub.observe_title(&buses, session_id, raw);
    }
    drop(subscription);
    let got = got.lock().unwrap().clone();
    (got, hub)
}

// v2 "normalizes an incoming semantic title before publication".
#[test]
fn control_characters_are_removed_before_publication() {
    assert_eq!(
        observed("title-controls", &["line1\ttab\rmid"]).0,
        ["line1tabmid"]
    );
}

// v2 "caps an oversized semantic title before fan-out".
#[test]
fn an_oversized_title_is_capped_before_fan_out() {
    assert_eq!(
        observed("title-huge", &[&"x".repeat(5_000)]).0,
        ["x".repeat(256)]
    );
}

// v2 "publishes only meaningful title changes".
#[test]
fn a_repeated_title_is_published_once() {
    assert_eq!(
        observed("title-dedupe", &["same", "same", "different"]).0,
        ["same", "different"]
    );
}

// v2 "collapses spinner animation while preserving state and label edges".
#[test]
fn spinner_animation_collapses_but_state_and_label_changes_publish() {
    let raw = [
        "π > waiting",
        "π ⠋ waiting",
        "π ⠙ waiting",
        "π ⠙ other task",
    ];
    assert_eq!(
        observed("title-spinner", &raw).0,
        ["π > waiting", "π ⠋ waiting", "π ⠙ other task"]
    );
}

// v2 "retains the displayed title rather than its deduplication key".
#[test]
fn the_retained_title_is_the_displayed_one_not_its_dedup_key() {
    let (_, hub) = observed("title-snapshot", &["π ⠸ shipping"]);
    let snapshot = hub.title_snapshot();
    let retained = snapshot
        .iter()
        .find(|entry| entry.session_id == "title-snapshot");
    assert_eq!(
        retained.map(|entry| entry.title.as_str()),
        Some("π ⠸ shipping")
    );
}

/// A generation that negotiated `terminal_metadata_v1`, with `CHANNEL` bound
/// to `SEMANTIC_SESSION`.
fn negotiated(fixture: &LinkFixture) -> WorkerFrameDispatcher {
    let handle = Arc::new(WorkerHandle::new(
        worker(WORKER_FP),
        None,
        "negotiated".to_owned(),
        std::collections::BTreeSet::from([CAPABILITY_TERMINAL_METADATA_V1.to_owned()]),
        fixture.socket.sender(),
    ));
    roost_coord::workers::registry::claim_generation(
        &fixture.services.buses,
        &fixture.services.workers,
        Arc::clone(&handle),
    );
    handle.mark_ready();
    fixture.services.byte_hub.bind_durable_channel(
        &worker(WORKER_FP),
        ChannelId::try_from(CHANNEL).unwrap(),
        &SessionId::try_from(SEMANTIC_SESSION).unwrap(),
    );
    fixture.services.worker_dispatcher(handle)
}

// v2 terminal-metadata-adapter.test.ts "projects negotiated title and activity
// metadata through the same hubs".
#[tokio::test]
async fn a_negotiated_metadata_frame_feeds_the_title_and_activity_hubs() {
    let fixture = LinkFixture::new("metadata-semantic").await;
    let dispatcher = negotiated(&fixture);
    let buses = &fixture.services.buses;
    let (titles, title_subscription) = collect(buses, SEMANTIC_SESSION);
    let activity = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&activity);
    let activity_subscription = buses.last_activity_bus.subscribe(move |message| {
        if message.session_id == SEMANTIC_SESSION {
            recorded.lock().unwrap().push(message.ts_ms);
        }
    });

    let metadata = CoordWorkerUpstream::TerminalMetadata(TerminalMetadata {
        channel_id: ChannelId::try_from(CHANNEL).unwrap(),
        title_changed: true,
        title: "semantic title".to_owned(),
        activity_changed: true,
        activity_ts_ms: 123,
        clipboard_changed: false,
        clipboard: String::new(),
        command_finished: false,
        command_exit_code: None,
        command_duration_ms: 0,
        bell: false,
        progress: None,
        notifications: Vec::new(),
        user_vars_changed: false,
        user_vars: Vec::new(),
    });
    let outcome = dispatcher.handle_now(WORKER_FP, live_frame(71, metadata));

    drop((title_subscription, activity_subscription));
    assert_eq!(outcome, DispatchOutcome::Handled);
    assert_eq!(*titles.lock().unwrap(), ["semantic title"]);
    assert_eq!(*activity.lock().unwrap(), [123]);
}

/// An OSC 52 write is an event: it reaches the clipboard bus once, as sent,
/// and leaves nothing behind that a later Sync link could be seeded with.
#[tokio::test]
async fn a_clipboard_write_is_published_once_and_retained_nowhere() {
    let fixture = LinkFixture::new("metadata-clipboard").await;
    let dispatcher = negotiated(&fixture);
    let buses = &fixture.services.buses;
    let (titles, title_subscription) = collect(buses, SEMANTIC_SESSION);
    let writes = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&writes);
    let clipboard_subscription = buses.clipboard_bus.subscribe(move |write| {
        recorded
            .lock()
            .unwrap()
            .push((write.session_id.clone(), write.text.clone()));
    });

    let metadata = CoordWorkerUpstream::TerminalMetadata(TerminalMetadata {
        channel_id: ChannelId::try_from(CHANNEL).unwrap(),
        title_changed: false,
        title: String::new(),
        activity_changed: false,
        activity_ts_ms: 0,
        clipboard_changed: true,
        clipboard: "git log --oneline".to_owned(),
        command_finished: false,
        command_exit_code: None,
        command_duration_ms: 0,
        bell: false,
        progress: None,
        notifications: Vec::new(),
        user_vars_changed: false,
        user_vars: Vec::new(),
    });
    let outcome = dispatcher.handle_now(WORKER_FP, live_frame(72, metadata));

    drop((title_subscription, clipboard_subscription));
    assert_eq!(outcome, DispatchOutcome::Handled);
    assert_eq!(
        *writes.lock().unwrap(),
        [(SEMANTIC_SESSION.to_owned(), "git log --oneline".to_owned())]
    );
    assert!(
        titles.lock().unwrap().is_empty(),
        "a clipboard-only record is not a title"
    );
    assert!(fixture.services.titles.title_snapshot().is_empty());
}

// v2 terminal-metadata-adapter.test.ts "drops input-direction and
// post-negotiation raw metadata frames".
#[tokio::test]
async fn a_raw_metadata_frame_publishes_no_title_in_either_direction() {
    let fixture = LinkFixture::new("metadata-gated").await;
    let dispatcher = negotiated(&fixture);
    let (titles, subscription) = collect(&fixture.services.buses, SEMANTIC_SESSION);
    let raw_title = b"\x1b]0;must not publish\x07".to_vec();

    for direction in [1, 0] {
        let raw = CoordWorkerUpstream::Binary(Binary {
            channel_id: ChannelId::try_from(CHANNEL).unwrap(),
            direction,
            data: raw_title.clone(),
            seq: 0,
        });
        assert_eq!(
            dispatcher.handle_now(WORKER_FP, live_frame(71, raw)),
            DispatchOutcome::Refused
        );
    }

    drop(subscription);
    assert!(titles.lock().unwrap().is_empty());
    assert!(fixture.services.titles.title_snapshot().is_empty());
}
