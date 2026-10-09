//! The coordinator's retained terminal signals: a repeat is not republished,
//! a clear forgets the report, the snapshot a fresh Sync link is seeded from
//! holds only live facts, and a notification passes through every time.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use roost_coord::events::bus_domains::Buses;
use roost_coord::events::bus_messages::SessionTerminalSignals;
use roost_coord::sync_ws::feed::signal_frames::session_terminal_signals_frame;
use roost_coord::terminal_screen::signal_hub::TerminalSignalHub;
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_protocol::terminal_signals::{TerminalNotification, TerminalProgress, TerminalUserVar};

fn recorder(buses: &Buses) -> (Arc<Mutex<Vec<SessionTerminalSignals>>>, impl Drop) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let subscription = buses
        .terminal_signal_bus
        .subscribe(move |signals| sink.lock().unwrap().push(signals.clone()));
    (seen, subscription)
}

fn vars(value: &str) -> Vec<TerminalUserVar> {
    vec![TerminalUserVar {
        key: "branch".to_owned(),
        value: value.to_owned(),
    }]
}

#[test]
fn repeats_are_swallowed_and_a_clear_forgets_the_report() {
    let buses = Buses::new();
    let (seen, _subscription) = recorder(&buses);
    let hub = TerminalSignalHub::new();

    hub.observe(
        &buses,
        "s",
        Some(TerminalProgress::Normal(10)),
        Some(vars("main")),
        None,
    );
    hub.observe(
        &buses,
        "s",
        Some(TerminalProgress::Normal(10)),
        Some(vars("main")),
        None,
    );
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "an unchanged repeat is not republished"
    );

    let snapshot = hub.snapshot();
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].progress, Some(TerminalProgress::Normal(10)));
    assert_eq!(snapshot[0].user_vars, Some(vars("main")));

    hub.observe(
        &buses,
        "s",
        Some(TerminalProgress::Clear),
        Some(Vec::new()),
        None,
    );
    assert_eq!(
        seen.lock().unwrap().len(),
        2,
        "the clear itself is published"
    );
    assert!(hub.snapshot().is_empty(), "a cleared session seeds nothing");
}

#[test]
fn notifications_pass_through_and_are_never_seeded() {
    let buses = Buses::new();
    let (seen, _subscription) = recorder(&buses);
    let hub = TerminalSignalHub::new();
    let done = TerminalNotification {
        title: String::new(),
        body: "Done".to_owned(),
    };
    hub.observe(&buses, "s", None, None, Some(done.clone()));
    hub.observe(&buses, "s", None, None, Some(done.clone()));
    assert_eq!(seen.lock().unwrap().len(), 2);
    assert!(hub.snapshot().is_empty());
}

#[test]
fn a_released_session_is_forgotten() {
    let buses = Buses::new();
    let hub = TerminalSignalHub::new();
    hub.observe(
        &buses,
        "s",
        Some(TerminalProgress::Indeterminate),
        None,
        None,
    );
    hub.release("s");
    assert!(hub.snapshot().is_empty());
}

#[test]
fn the_sync_frame_carries_only_what_changed() {
    let frame = session_terminal_signals_frame(&SessionTerminalSignals {
        session_id: "s".to_owned(),
        progress: Some(TerminalProgress::Paused(Some(55))),
        user_vars: None,
        notification: None,
    });
    let Some(Frame::TerminalSignals(signals)) = frame.frame().frame.clone() else {
        panic!("not a terminal signals frame");
    };
    assert!(signals.progress_present);
    assert_eq!(
        (signals.progress_state, signals.progress_percent),
        (4, Some(55))
    );
    assert!(!signals.user_vars_present);
    assert!(!signals.notification_present);
}
