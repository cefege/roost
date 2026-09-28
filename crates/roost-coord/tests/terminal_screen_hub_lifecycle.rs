//! The screen hub's watcher lifecycle under re-entry and session close: a
//! socket's callbacks may register sockets and watch sessions while the hub is
//! retiring the old ones, neither the retiring registration nor a stale
//! callback may corrupt the watcher index, and a closed session releases its
//! watchers and its resident bytes.
//!
//! Ports `apps/coord/tests/terminal/screen/terminal-screen-hub-lifecycle.test.ts`
//! and the two watcher cases of `terminal-screen-hub.test.ts`. The index is
//! observed through fan-out: a socket watches a session iff it hears that
//! session's next stream. v2's "detaches every registration before disposing
//! reentrant callbacks" is not ported: `dispose` runs only at v2 process exit
//! (`main.ts:260`), and the Rust hub is dropped with its process.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod terminal_screen_hub_support;

use std::sync::Arc;

use roost_coord::events::bus_domains::Buses;
use roost_coord::events::bus_messages::SessionBusMessage;
use roost_coord::terminal_screen::ScreenHub;
use roost_coord::terminal_screen::hub_contract::TerminalScreenSocketSink;
use roost_coord::terminal_screen::screen_budget::TerminalScreenCaps;
use roost_coord::terminal_view::TerminalViewHub;
use roost_protocol::wire::{SessionEvent, SessionId};
use terminal_screen_hub_support::{
    OTHER_SESSION, OTHER_STREAM, SESSION, STREAM, TestSink, baseline, delta, full_frame, harness,
    harness_with_caps, other_session, session,
};

const THIRD_STREAM: &str = "50000000-0000-4000-8000-000000000003";

fn register(hub: &Arc<ScreenHub>, socket_id: &str, sink: &Arc<TestSink>) {
    hub.register_socket(
        socket_id,
        Arc::clone(sink) as Arc<dyn TerminalScreenSocketSink>,
    );
}

fn heard(sink: &TestSink, session_id: &str, stream_id: &str) -> bool {
    sink.begins()
        .contains(&(session_id.to_owned(), stream_id.to_owned()))
}

// v2 terminal-screen-hub.test.ts "indexes watcher lifecycle through unwatch,
// replacement, retirement, and session drop without scanning unrelated
// sockets".
#[test]
fn replacing_a_socket_keeps_the_registration_its_retirement_installed() {
    let h = harness();
    let view = TestSink::queuing();
    let retired = TestSink::queuing();
    register(&h.hub, "view", &view);
    register(&h.hub, "retired", &retired);
    h.hub.set_watching("view", &session(), true);
    h.hub.set_watching("view", &other_session(), true);
    h.hub.set_watching("retired", &session(), true);
    h.hub.expect_stream(&session(), STREAM, 8, 2);
    h.frame(baseline(1, &[]));
    h.frame(delta(1, "changed"));
    h.hub.set_watching("view", &session(), false);
    assert_eq!(view.drops(), [SESSION]);

    let reentered = TestSink::queuing();
    let hub = Arc::clone(&h.hub);
    let installed = Arc::clone(&reentered);
    view.on_drop(Arc::new(move |_: &SessionId| {
        hub.register_socket(
            "view",
            Arc::clone(&installed) as Arc<dyn TerminalScreenSocketSink>,
        );
        hub.set_watching("view", &session(), true);
    }));
    let replacement = TestSink::queuing();
    register(&h.hub, "view", &replacement);
    assert_eq!(view.drops(), [SESSION, OTHER_SESSION]);
    assert!(reentered.drops().is_empty());

    h.frame(delta(2, "after"));
    assert_eq!(
        reentered.delta_texts(),
        [["after"]],
        "the registration its retirement installed watches"
    );
    assert_eq!(retired.deltas.lock().unwrap().len(), 2);
    assert!(
        replacement.events().is_empty(),
        "the reentrant registration was not overwritten"
    );
    h.hub.expect_stream(&other_session(), OTHER_STREAM, 8, 2);
    assert!(
        !heard(&reentered, OTHER_SESSION, OTHER_STREAM),
        "the old watch of OTHER went with its socket"
    );

    h.hub.unregister_socket("retired");
    assert_eq!(retired.drops(), [SESSION]);
    h.hub.unregister_socket("view");
    assert_eq!(reentered.drops(), [SESSION]);
    assert!(replacement.drops().is_empty());

    let closing = TestSink::queuing();
    register(&h.hub, "closing", &closing);
    h.hub.set_watching("closing", &session(), true);
    h.hub.drop_session(&session());
    assert_eq!(closing.drops(), [SESSION]);
}

// v2 terminal-screen-hub.test.ts "copies watcher IDs before reentrant delta
// callbacks".
#[test]
fn a_delta_never_reaches_a_socket_unregistered_during_its_fan_out() {
    let h = harness();
    let first = TestSink::queuing();
    let second = TestSink::queuing();
    let late = TestSink::queuing();
    let hub = Arc::clone(&h.hub);
    let joining = Arc::clone(&late);
    first.on_delta(Arc::new(move |_: &SessionId| {
        hub.unregister_socket("second");
        hub.register_socket(
            "late",
            Arc::clone(&joining) as Arc<dyn TerminalScreenSocketSink>,
        );
        hub.set_watching("late", &session(), true);
    }));
    register(&h.hub, "first", &first);
    register(&h.hub, "second", &second);
    h.hub.set_watching("first", &session(), true);
    h.hub.set_watching("second", &session(), true);
    h.hub.expect_stream(&session(), STREAM, 8, 2);
    h.frame(baseline(1, &[]));
    h.frame(delta(1, "changed"));

    assert_eq!(first.deltas.lock().unwrap().len(), 1);
    assert!(
        second.deltas.lock().unwrap().is_empty(),
        "second was gone before its turn"
    );
    assert_eq!(second.drops(), [SESSION]);
    assert!(
        late.deltas.lock().unwrap().is_empty(),
        "late joined after the fan-out began"
    );
}

// v2 terminal-screen-hub-lifecycle.test.ts "detaches every old socket watch
// before callback and preserves a reentrant replacement".
#[test]
fn unregistering_detaches_every_watch_before_a_callback_reregisters_the_id() {
    let h = harness();
    let stale = TestSink::queuing();
    let replacement = TestSink::queuing();
    register(&h.hub, "socket", &stale);
    h.hub.set_watching("socket", &session(), true);
    h.hub.set_watching("socket", &other_session(), true);
    let hub = Arc::clone(&h.hub);
    let installed = Arc::clone(&replacement);
    stale.on_drop(Arc::new(move |dropped: &SessionId| {
        if dropped.as_str() != SESSION {
            return;
        }
        hub.register_socket(
            "socket",
            Arc::clone(&installed) as Arc<dyn TerminalScreenSocketSink>,
        );
        hub.set_watching("socket", &other_session(), true);
    }));

    h.hub.unregister_socket("socket");

    assert_eq!(stale.drops(), [SESSION, OTHER_SESSION]);
    h.hub.expect_stream(&session(), STREAM, 8, 2);
    h.hub.expect_stream(&other_session(), OTHER_STREAM, 8, 2);
    assert!(
        !heard(&replacement, SESSION, STREAM),
        "nobody watches SESSION any more"
    );
    assert!(
        heard(&replacement, OTHER_SESSION, OTHER_STREAM),
        "the replacement's own watch survives"
    );
    assert!(
        stale.begins().is_empty(),
        "the retired sink hears nothing further"
    );
}

// v2 terminal-screen-hub-lifecycle.test.ts "clears watches reinstalled by
// stale session-drop callbacks".
#[test]
fn dropping_a_session_clears_a_watch_its_own_callbacks_reinstalled() {
    let h = harness();
    let first = TestSink::queuing();
    let second = TestSink::queuing();
    let reentrant = TestSink::queuing();
    h.hub.expect_stream(&session(), STREAM, 8, 2);
    register(&h.hub, "first", &first);
    register(&h.hub, "second", &second);
    h.hub.set_watching("first", &session(), true);
    h.hub.set_watching("second", &session(), true);
    let hub = Arc::clone(&h.hub);
    let joining = Arc::clone(&reentrant);
    first.on_drop(Arc::new(move |dropped: &SessionId| {
        hub.register_socket(
            "reentrant",
            Arc::clone(&joining) as Arc<dyn TerminalScreenSocketSink>,
        );
        hub.set_watching("reentrant", dropped, true);
    }));

    h.hub.drop_session(&session());

    assert_eq!(first.drops(), [SESSION]);
    assert_eq!(second.drops(), [SESSION]);
    assert!(reentrant.begins().is_empty());
    h.hub.expect_stream(&session(), THIRD_STREAM, 8, 2);
    assert!(
        !heard(&reentrant, SESSION, THIRD_STREAM),
        "a watch a stale drop callback reinstalled does not outlive the drop"
    );
}

// v2 terminal-view-hub.ts:258-260 subscribes the session bus and closes a
// `closed` session, which ends in `screen.dropSession`; the retired-worker path
// (`workerRetired`) closes the same way. The budget below holds one session's
// viewport, so the second baseline fits only if the closed one was released.
#[test]
fn a_closed_session_releases_its_watchers_and_its_resident_bytes() {
    let h = harness_with_caps(TerminalScreenCaps {
        max_resident_rows: 2,
        max_resident_spans: 2,
    });
    let views = Arc::new(TerminalViewHub::new());
    views.set_screens(Arc::downgrade(&h.hub));
    let buses = Buses::shared();
    let _release = views.subscribe_session_close(&buses);
    let sink = TestSink::queuing();
    register(&h.hub, "socket-a", &sink);
    h.hub.set_watching("socket-a", &session(), true);
    h.hub.expect_stream(&session(), STREAM, 8, 2);
    h.frame(baseline(1, &[]));

    let closed = SessionEvent::Closed {
        session_id: session(),
        exit_code: None,
        ts: 1,
        trace_id: None,
    };
    buses
        .session_bus
        .publish(SessionBusMessage::committed(closed, 1));

    assert_eq!(sink.drops(), [SESSION]);
    assert_eq!(h.hub.expected_stream_id(&session()), None);
    h.hub.expect_stream(&other_session(), OTHER_STREAM, 8, 2);
    h.hub.publish_frame(
        &other_session(),
        &mut full_frame(OTHER_STREAM, 1, 8, 2, &[]),
        0,
    );
    assert!(
        h.unavailable().is_empty(),
        "the closed session's bytes went back to the pool"
    );
    assert!(h.hub.has_valid_cache(&other_session()));
}
