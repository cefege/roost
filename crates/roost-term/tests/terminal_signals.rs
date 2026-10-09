//! Program-to-operator signals the core parses: OSC 9;4 progress, OSC 9 and
//! OSC 777 notifications, and OSC 1337 SetUserVar. Live output reports them
//! once; a replay never does.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_term::{RioCore, TerminalCore, TerminalNotification, TerminalProgress, TerminalUserVar};

#[test]
fn a_progress_report_is_taken_once() {
    let mut core = RioCore::new(80, 24);
    core.write_raw(b"\x1b]9;4;1;42\x07");
    assert_eq!(core.take_progress(), Some(TerminalProgress::Normal(42)));
    assert_eq!(core.take_progress(), None);

    core.write_raw(b"\x1b]9;4;3\x07\x1b]9;4;0\x07");
    assert_eq!(
        core.take_progress(),
        Some(TerminalProgress::Clear),
        "the latest wins"
    );

    core.write_raw(b"\x1b]9;4;2;70\x07");
    assert_eq!(
        core.take_progress(),
        Some(TerminalProgress::Error(Some(70)))
    );
}

#[test]
fn a_replay_reports_nothing() {
    let mut core = RioCore::new(80, 24);
    core.write(b"\x1b]9;4;1;10\x07\x1b]9;Done\x07");
    assert_eq!(core.take_progress(), None);
    assert!(core.take_desktop_notifications().is_empty());
}

#[test]
fn osc_9_and_777_notify_but_conemu_subcommands_do_not() {
    let mut core = RioCore::new(80, 24);
    core.write_raw(b"\x1b]9;Done\x07\x1b]9;1;500\x07\x1b]777;notify;Build;finished\x07");
    assert_eq!(
        core.take_desktop_notifications(),
        vec![
            TerminalNotification {
                title: String::new(),
                body: "Done".to_owned()
            },
            TerminalNotification {
                title: "Build".to_owned(),
                body: "finished".to_owned()
            },
        ]
    );
    assert!(core.take_desktop_notifications().is_empty());
}

#[test]
fn a_burst_of_notifications_is_capped() {
    let mut core = RioCore::new(80, 24);
    let burst: Vec<u8> = (0..20)
        .flat_map(|index| format!("\x1b]9;n{index}\x07").into_bytes())
        .collect();
    core.write_raw(&burst);
    assert_eq!(core.take_desktop_notifications().len(), 8);
}

#[test]
fn a_user_var_is_published_and_flags_its_change_once() {
    let mut core = RioCore::new(80, 24);
    assert!(!core.take_user_vars_changed());
    core.write_raw(b"\x1b]1337;SetUserVar=branch=bWFpbg==\x07");
    assert!(core.take_user_vars_changed());
    assert!(!core.take_user_vars_changed());
    assert_eq!(
        core.user_vars(),
        vec![TerminalUserVar {
            key: "branch".to_owned(),
            value: "main".to_owned()
        }]
    );
    core.write_raw(b"\x1b]1337;SetUserVar=branch=bWFpbg==\x07");
    assert!(
        !core.take_user_vars_changed(),
        "an unchanged value is no change"
    );
}
