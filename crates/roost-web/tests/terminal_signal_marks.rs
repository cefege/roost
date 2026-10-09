//! How a session's program signals present: the progress bar's fill, state and
//! label per OSC 9;4 state, and the user-variable badges' truncation.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_protocol::terminal_signals::{TerminalProgress, TerminalUserVar};
use roost_web::components::terminal_signal_marks::{progress_presentation, user_var_badges};

#[test]
fn each_progress_state_presents_its_fill_and_colour() {
    assert_eq!(progress_presentation(TerminalProgress::Clear), None);
    assert_eq!(
        progress_presentation(TerminalProgress::Normal(40)),
        Some((Some(0.4), "normal", "Progress 40%".to_owned()))
    );
    assert_eq!(
        progress_presentation(TerminalProgress::Indeterminate),
        Some((None, "busy", "Working".to_owned()))
    );
    assert_eq!(
        progress_presentation(TerminalProgress::Error(None)),
        Some((Some(1.0), "error", "Failed".to_owned()))
    );
    assert_eq!(
        progress_presentation(TerminalProgress::Paused(Some(70))),
        Some((Some(0.7), "paused", "Paused at 70%".to_owned()))
    );
}

#[test]
fn three_badges_show_and_the_rest_are_in_the_tooltip() {
    let vars: Vec<TerminalUserVar> = ["a", "b", "c", "d"]
        .iter()
        .map(|key| TerminalUserVar {
            key: (*key).to_owned(),
            value: "1".to_owned(),
        })
        .collect();
    let (shown, all) = user_var_badges(&vars);
    assert_eq!(shown, ["a: 1", "b: 1", "c: 1"]);
    assert_eq!(all.as_deref(), Some("a: 1\nb: 1\nc: 1\nd: 1"));
    let (shown, all) = user_var_badges(&vars[..2]);
    assert_eq!(shown.len(), 2);
    assert_eq!(all, None);
}
