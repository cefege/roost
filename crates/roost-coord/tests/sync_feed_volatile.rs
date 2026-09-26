//! The volatile half of the firehose: session presence, last activity, and the
//! browser-only UI stream.
//!
//! These three are in a file of their own because they are the three adapters
//! whose fan-out rule is NOT "everyone watching". Presence is broadcast to every
//! viewer of a session; a UI layout is addressed to one socket; an activity
//! observation is broadcast but throttled so a streaming terminal cannot push
//! every other domain off its socket. Flattening them into one generic adapter
//! is how a presence change ends up scoped to a single browser, and how one
//! tab's acknowledged layout ends up executing in another.
//!
//! Every unwrap here is an assertion over a value the test just built: the panic
//! IS the failure, which is why `unwrap_used` is denied in product code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod sync_feed_support;

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use roost_coord::events::bus_domains::Buses;
use roost_coord::events::bus_messages::{
    LastActivityUpdate, SessionBusMessage, SessionPresenceUpdate, UiBusMsg,
};
use roost_coord::sync_ws::feed::last_activity::{
    LAST_ACTIVITY_THROTTLE_MS, LastActivityHub, last_activity_frame,
};
use roost_coord::sync_ws::feed::presence::{
    presence_is_addressed_to_another_viewer, publish_presence, session_presence_frame,
};
use roost_coord::sync_ws::feed::ui::{UiViewer, ui_bus_frame, ui_state_seed_frames};
use roost_coord::sync_ws::frame_meta::{FeedLane, SyncFrameMeta};
use roost_coord::sync_ws::frame_meta::SyncDomain;
use roost_coord::ui_state::state_owner::UiStateOwner;
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::UiCommand;
use roost_protocol::wire::{ChannelId, SessionEvent};
use serde_json::json;

use sync_feed_support::{SESSION_A, WORKER_A, oneof_of, session, worker, FixedRoutes};

#[test]
fn a_busy_session_fans_out_at_most_one_activity_frame_per_throttle_window() {
    let buses = Buses::default();
    let now = Arc::new(AtomicI64::new(1_700_000_000_000));
    let hub = LastActivityHub::with_clock(Arc::new({
        let now = Arc::clone(&now);
        move || now.load(Ordering::Relaxed)
    }));

    assert!(
        hub.observe_and_publish(&buses, SESSION_A, 1_700_000_000_000),
        "the first observation for a session has no value to be stale, so it \
         must publish rather than leave the session looking idle"
    );

    // The worker's own stamp jumps a whole window; the throttle must still be
    // measured against LOCAL receipt time, or a fast worker clock publishes on
    // every byte of output.
    now.fetch_add(LAST_ACTIVITY_THROTTLE_MS - 1, Ordering::Relaxed);
    assert!(
        !hub.observe_and_publish(&buses, SESSION_A, 1_700_009_000_000),
        "one millisecond short of the window must still be throttled"
    );
    assert_eq!(
        hub.snapshot(),
        vec![LastActivityUpdate {
            session_id: SESSION_A.to_owned(),
            ts_ms: 1_700_009_000_000,
        }],
        "the retained value tracks the newest observation even while the frame \
         is throttled, or a fresh subscriber is seeded with a stale timestamp"
    );

    now.fetch_add(1, Ordering::Relaxed);
    assert!(
        hub.observe_and_publish(&buses, SESSION_A, 1_700_009_000_001),
        "the throttle opens once the window has fully elapsed"
    );
    assert_eq!(buses.last_activity_bus.retained_count(), 2);
}

#[test]
fn a_closed_session_releases_its_retained_activity() {
    let buses = Buses::default();
    let hub = Arc::new(LastActivityHub::new());
    let _subscription = hub.subscribe_session_close(&buses);
    assert!(hub.observe_and_publish(&buses, SESSION_A, 1_700_000_000_000));
    assert_eq!(hub.retained(), 1);

    buses.session_bus.publish(SessionBusMessage::committed(
        SessionEvent::Renamed {
            session_id: session(SESSION_A),
            custom_title: "renamed".to_owned(),
            ts: 1_700_000_000_001,
            trace_id: None,
        },
        3,
    ));
    assert_eq!(
        hub.retained(),
        1,
        "a rename is not a close, so the session keeps its activity"
    );

    buses
        .session_bus
        .publish(sync_feed_support::closed_message());
    assert_eq!(
        hub.retained(),
        0,
        "a closed session must release its activity, or a reused session id \
         inherits the previous one's last-moved timestamp"
    );
}

#[test]
fn presence_reaches_every_viewer_except_the_one_that_authored_it() {
    let mine = json!({"kind":"presence-delta","viewer_id":"fingerprint:tab-1","cursor_col":3});
    let theirs = json!({"kind":"presence-delta","viewer_id":"fingerprint:tab-2","cursor_col":9});
    let viewers = json!({"kind":"viewers","fps":["fingerprint:tab-1","fingerprint:tab-2"]});

    assert!(
        presence_is_addressed_to_another_viewer(&mine, Some("fingerprint:tab-1")),
        "a viewer's own cursor notice is the one thing it already knows"
    );
    assert!(
        presence_is_addressed_to_another_viewer(&theirs, Some("fingerprint:tab-1")),
        "another viewer's cursor is the feature, so it must NOT be filtered: a \
         presence change is broadcast to every viewer of the session"
    );
    assert!(!presence_is_addressed_to_another_viewer(
        &viewers,
        Some("fingerprint:tab-1")
    ));
    assert!(
        !presence_is_addressed_to_another_viewer(&mine, None),
        "a socket with no viewer identity cannot have authored a notice that names one"
    );
    assert!(
        !presence_is_addressed_to_another_viewer(&json!("a string"), Some("x")),
        "an opaque non-object payload is addressed to nobody"
    );

    let frame = session_presence_frame(&SessionPresenceUpdate {
        session_id: SESSION_A.to_owned(),
        data: mine.clone(),
    });
    match oneof_of(&frame) {
        Frame::SessionPresence(presence) => {
            assert_eq!(presence.session_id, SESSION_A);
            assert_eq!(presence.payload_json, mine.to_string());
        }
        other => panic!("presence must produce a presence frame, got {other:?}"),
    }
    assert_eq!(
        frame.meta(),
        &SyncFrameMeta {
            domain: Some(SyncDomain::Terminal),
            lane: FeedLane::Session,
            session_id: Some(SESSION_A.to_owned()),
            ..SyncFrameMeta::default()
        }
    );
}

#[test]
fn a_relayed_presence_payload_is_keyed_by_the_session_its_channel_carries() {
    let channel = ChannelId::try_from(4_i64).expect("channel 4 is valid");
    let routes = FixedRoutes::new(worker(WORKER_A), channel, session(SESSION_A));
    let buses = Buses::default();

    assert_eq!(
        publish_presence(
            &buses,
            &routes,
            &worker(WORKER_A),
            channel,
            json!({"kind":"presence-delta"})
        ),
        Some(session(SESSION_A)),
        "a channel the route index still carries is published against its \
         session, which is the re-keying the SPA's single subscription needs"
    );
    assert_eq!(
        buses.global_presence_bus.retained_count(),
        1,
        "the re-keyed payload must reach the bus a Sync socket subscribes to"
    );
    assert_eq!(
        publish_presence(
            &buses,
            &routes,
            &worker(WORKER_A),
            ChannelId::try_from(9_i64).expect("channel 9 is valid"),
            json!({"kind":"presence-delta"})
        ),
        None,
        "a channel no live session carries is dropped at the door: a presence \
         notice about a session the reader has never been told about is \
         indistinguishable from one about a session that does not exist"
    );
    assert_eq!(buses.global_presence_bus.retained_count(), 1);
}

#[test]
fn a_ui_apply_reaches_only_the_socket_that_acknowledged_it() {
    let apply = UiBusMsg::Apply {
        target_tab_id: "tab-1".to_owned(),
        target_socket_id: "socket-2".to_owned(),
        correlation_id: "corr-1".to_owned(),
        command: UiCommand::default(),
    };
    let legacy = UiBusMsg::Command {
        target_tab_id: "tab-1".to_owned(),
        command: UiCommand::default(),
    };

    assert!(
        ui_bus_frame(&apply, &UiViewer::suppressed()).is_none(),
        "a worker socket and a read-only feed stay subscribed for delivery \
         counts while dropping every live UI frame"
    );
    assert!(
        ui_bus_frame(&apply, &UiViewer::browser("socket-1")).is_none(),
        "an acknowledged apply is addressed to the socket that acknowledged it, \
         so another browser must not execute this tab's layout"
    );
    let addressed =
        ui_bus_frame(&apply, &UiViewer::browser("socket-2")).expect("the acking socket gets it");
    match oneof_of(&addressed) {
        Frame::UiCommand(sent) => {
            assert_eq!(sent.target_tab_id, "tab-1");
            assert_eq!(sent.correlation_id, "corr-1");
            assert_eq!(sent.target_socket_id, "socket-2");
        }
        other => panic!("an apply must produce a command frame, got {other:?}"),
    }
    assert_eq!(
        addressed.meta().domain,
        None,
        "UI traffic is unsequenced: it must not consume the application window"
    );

    // A legacy command has no addressee, so it broadcasts to every browser.
    for socket in ["socket-1", "socket-2"] {
        let broadcast = ui_bus_frame(&legacy, &UiViewer::browser(socket))
            .expect("a legacy command reaches every browser");
        match oneof_of(&broadcast) {
            Frame::UiCommand(sent) => {
                assert!(sent.correlation_id.is_empty());
                assert!(sent.target_socket_id.is_empty());
            }
            other => panic!("a legacy command must produce a command frame, got {other:?}"),
        }
    }
}

#[test]
fn the_retained_ui_reports_seed_a_fresh_browser_and_the_bus_replays_nothing() {
    let states = UiStateOwner::new();
    states
        .report(
            "fingerprint",
            "tab-1",
            roost_proto::UiReportStateRequest::default(),
        )
        .expect("a first report is admitted");

    let seed = ui_state_seed_frames(&states);
    assert_eq!(seed.len(), 1);
    match oneof_of(&seed[0]) {
        Frame::UiState(state) => {
            assert_eq!(state.fp, "fingerprint");
            assert_eq!(state.tab_id, "tab-1");
        }
        other => panic!("the UI seed must be a state frame, got {other:?}"),
    }

    let buses = Buses::default();
    assert_eq!(
        buses.ui_bus.capacity(),
        0,
        "the UI bus retains nothing, so the seed is the only way a returning \
         browser learns which tabs exist"
    );
}

#[test]
fn an_activity_frame_is_session_keyed_and_lands_in_the_session_lane() {
    let frame = last_activity_frame(&LastActivityUpdate {
        session_id: SESSION_A.to_owned(),
        ts_ms: 1_700_000_000_000,
    });
    assert_eq!(frame.meta().session_id.as_deref(), Some(SESSION_A));
    assert_eq!(frame.meta().lane, FeedLane::Session);
    assert_eq!(frame.meta().domain, Some(SyncDomain::Terminal));
}
