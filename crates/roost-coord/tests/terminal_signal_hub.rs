//! The coordinator's retained terminal signals: a repeat is not republished,
//! a clear forgets the report, a closed session keeps only its done and error
//! program status records, the snapshot a fresh Sync link is seeded from holds
//! only live facts, and a notification passes through every time.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use roost_coord::events::bus_domains::Buses;
use roost_coord::events::bus_messages::SessionTerminalSignals;
use roost_coord::sync_ws::feed::signal_frames::session_terminal_signals_frame;
use roost_coord::terminal_screen::signal_hub::TerminalSignalHub;
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_protocol::program_status::{ProgramState, ProgramStatusRecord};
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
fn a_closed_session_drops_progress_and_user_vars() {
    let buses = Buses::new();
    let hub = TerminalSignalHub::new();
    hub.observe(
        &buses,
        "s",
        Some(TerminalProgress::Indeterminate),
        Some(vars("main")),
        None,
    );
    hub.session_closed(&buses, "s");
    assert!(hub.snapshot().is_empty());
}

#[test]
fn a_closed_session_keeps_done_and_error_and_drops_working_blocked_and_idle() {
    let buses = Buses::new();
    let (seen, _subscription) = recorder(&buses);
    let hub = TerminalSignalHub::new();
    hub.observe_program_status(
        &buses,
        "s",
        vec![
            program_record("build", ProgramState::Done),
            program_record("test", ProgramState::Working),
            program_record("deploy", ProgramState::Blocked),
            program_record("lint", ProgramState::Idle),
            program_record("fmt", ProgramState::Error),
        ],
    );
    hub.session_closed(&buses, "s");

    let published = seen.lock().unwrap().last().unwrap().program_status.clone();
    let kept = published.expect("the surviving set is published");
    let mut ids: Vec<String> = kept
        .iter()
        .map(|record| record.id.clone().unwrap())
        .collect();
    ids.sort();
    assert_eq!(ids, vec!["build".to_owned(), "fmt".to_owned()]);
    assert!(
        kept.iter()
            .all(|record| matches!(record.state, ProgramState::Done | ProgramState::Error))
    );
    assert_eq!(hub.snapshot()[0].program_status, Some(kept));
}

#[test]
fn a_closed_session_with_only_working_records_publishes_an_empty_set() {
    let buses = Buses::new();
    let (seen, _subscription) = recorder(&buses);
    let hub = TerminalSignalHub::new();
    hub.observe_program_status(
        &buses,
        "s",
        vec![program_record("test", ProgramState::Working)],
    );
    hub.session_closed(&buses, "s");
    assert_eq!(seen.lock().unwrap().len(), 2);
    assert_eq!(seen.lock().unwrap()[1].program_status, Some(Vec::new()));
    assert!(hub.snapshot().is_empty());

    hub.session_closed(&buses, "s");
    assert_eq!(
        seen.lock().unwrap().len(),
        2,
        "a second close changes nothing"
    );
}

#[test]
fn the_sync_frame_carries_only_what_changed() {
    let frame = session_terminal_signals_frame(&SessionTerminalSignals {
        session_id: "s".to_owned(),
        program_status: None,
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

fn program_record(id: &str, state: ProgramState) -> ProgramStatusRecord {
    ProgramStatusRecord {
        id: Some(id.to_owned()),
        state,
        kind: None,
        app: None,
        title: None,
        msg: None,
        progress: None,
    }
}

#[test]
fn program_status_is_replaced_deduplicated_and_cleared_by_an_empty_set() {
    let buses = Buses::new();
    let (seen, _subscription) = recorder(&buses);
    let hub = TerminalSignalHub::new();
    let working = vec![program_record("build", ProgramState::Working)];

    hub.observe_program_status(&buses, "s", working.clone());
    hub.observe_program_status(&buses, "s", working.clone());
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "a repeated set is not republished"
    );
    assert_eq!(
        seen.lock().unwrap()[0].program_status,
        Some(working.clone())
    );
    assert_eq!(hub.snapshot()[0].program_status, Some(working));

    hub.observe_program_status(&buses, "s", Vec::new());
    assert_eq!(seen.lock().unwrap()[1].program_status, Some(Vec::new()));
    assert!(hub.snapshot().is_empty(), "an empty set seeds nothing");
}

#[test]
fn the_sync_frame_carries_the_whole_program_status_set() {
    let frame = session_terminal_signals_frame(&SessionTerminalSignals {
        session_id: "s".to_owned(),
        program_status: Some(vec![program_record("build", ProgramState::Blocked)]),
        progress: None,
        user_vars: None,
        notification: None,
    });
    let Some(Frame::TerminalSignals(signals)) = frame.frame().frame.clone() else {
        panic!("not a terminal signals frame");
    };
    assert!(signals.program_status_present);
    assert_eq!(signals.program_status.len(), 1);
    assert_eq!(signals.program_status[0].state, "blocked");
    assert_eq!(signals.program_status[0].id.as_deref(), Some("build"));
}
