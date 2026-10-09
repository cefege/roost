//! A session's program signals beside its tab title and sidebar row: the
//! OSC 9;4 progress report as a compact bar, and the shell's OSC 1337 user
//! variables as small badges. Reads `Store::terminal_signals`; rendered by
//! `PaneTab` and `SessionRowFlat`.

use dioxus::prelude::*;
use roost_protocol::terminal_signals::{TerminalProgress, TerminalUserVar};

use crate::components::md::{Chip, ProgressBar};
use crate::pump::use_store;

/// Badges shown inline; the rest are listed in the last badge's tooltip.
pub const USER_VAR_BADGES_SHOWN: usize = 3;

/// What the bar shows for a report: the fill fraction (`None` = indeterminate
/// sweep), the `data-state` the stylesheet colours by, and the accessible
/// label. `None` when there is nothing to show.
pub fn progress_presentation(
    progress: TerminalProgress,
) -> Option<(Option<f64>, &'static str, String)> {
    let fraction = |percent: u8| f64::from(percent) / 100.0;
    match progress {
        TerminalProgress::Clear => None,
        TerminalProgress::Normal(percent) => Some((
            Some(fraction(percent)),
            "normal",
            format!("Progress {percent}%"),
        )),
        TerminalProgress::Error(percent) => Some((
            Some(fraction(percent.unwrap_or(100))),
            "error",
            match percent {
                Some(percent) => format!("Failed at {percent}%"),
                None => "Failed".to_owned(),
            },
        )),
        TerminalProgress::Indeterminate => Some((None, "busy", "Working".to_owned())),
        TerminalProgress::Paused(percent) => Some((
            Some(fraction(percent.unwrap_or(0))),
            "paused",
            match percent {
                Some(percent) => format!("Paused at {percent}%"),
                None => "Paused".to_owned(),
            },
        )),
    }
}

/// The badges to show inline, and the tooltip listing every variable when
/// some are hidden.
pub fn user_var_badges(vars: &[TerminalUserVar]) -> (Vec<String>, Option<String>) {
    let shown = vars
        .iter()
        .take(USER_VAR_BADGES_SHOWN)
        .map(|var| format!("{}: {}", var.key, var.value))
        .collect();
    let all = (vars.len() > USER_VAR_BADGES_SHOWN).then(|| {
        vars.iter()
            .map(|var| format!("{}: {}", var.key, var.value))
            .collect::<Vec<_>>()
            .join("\n")
    });
    (shown, all)
}

/// The progress bar, or nothing when the session reports none.
#[component]
pub fn TerminalProgressMark(session_id: String) -> Element {
    let pump = use_store();
    let _ = pump.revision().read();
    let progress = pump
        .core()
        .borrow()
        .store()
        .terminal_signals
        .progress(&session_id);
    let Some((value, state, label)) = progress.and_then(progress_presentation) else {
        return rsx! {};
    };
    rsx! {
        span {
            class: "terminal-progress-mark",
            "data-state": state,
            "data-testid": "terminal-progress-{session_id}",
            title: label.clone(),
            ProgressBar { value, label: label.clone() }
        }
    }
}

/// The user-variable badges, or nothing when the shell published none.
#[component]
pub fn TerminalUserVarBadges(session_id: String) -> Element {
    let pump = use_store();
    let _ = pump.revision().read();
    let vars = pump
        .core()
        .borrow()
        .store()
        .terminal_signals
        .user_vars(&session_id)
        .to_vec();
    if vars.is_empty() {
        return rsx! {};
    }
    let (shown, all) = user_var_badges(&vars);
    let last = shown.len().saturating_sub(1);
    rsx! {
        span { class: "terminal-user-vars", "data-testid": "terminal-user-vars-{session_id}",
            for (index, badge) in shown.into_iter().enumerate() {
                Chip {
                    key: "{index}",
                    small: true,
                    title: if index == last { all.clone().unwrap_or_else(|| badge.clone()) } else { badge.clone() },
                    label: badge,
                }
            }
        }
    }
}
