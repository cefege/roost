//! The live working row: an activity indicator, elapsed time since the row
//! appeared, and the running tool's intent (or "Working…"/"Writing…").
//! Pinned above the composer so it stays visible throughout the run. Reduced
//! motion disables the indicator animation and ticks elapsed time once a second.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{INTENT_FIELD, TranscriptItem, intent_label};

use crate::components::terminal::dom::{now_ms, sleep_ms};
#[cfg(target_arch = "wasm32")]
use crate::motion::view_transition::prefers_reduced_motion;
use libero::components::{Flex, Indicator, Text};
use libero::sx::sx;

const FRAME_TICK_MS: u64 = 33;
const REDUCED_TICK_MS: u64 = 1_000;
const IDLE_LABEL: &str = "Working…";

/// The intent of the most recent running tool, else the idle label. A running
/// tool without an intent shows the idle label rather than an older tool's.
pub(super) fn working_label(items: &[TranscriptItem]) -> String {
    let running_args = items.iter().rev().find_map(|item| match item {
        TranscriptItem::Tool {
            running: true,
            args_json,
            ..
        } => Some(args_json.as_str()),
        _ => None,
    });
    running_args
        .and_then(intent_from_args)
        .unwrap_or_else(|| IDLE_LABEL.to_owned())
}

fn intent_from_args(args_json: &str) -> Option<String> {
    let arguments: serde_json::Value = serde_json::from_str(args_json).ok()?;
    intent_label(arguments.get(INTENT_FIELD)?.as_str()?)
}

/// OMP's short duration: `123ms`, `1.5s`, `30m15s`, `2h30m`, `3d2h`.
pub(super) fn format_duration(ms: u64) -> String {
    const SEC: u64 = 1_000;
    const MIN: u64 = 60 * SEC;
    const HOUR: u64 = 60 * MIN;
    const DAY: u64 = 24 * HOUR;
    if ms < SEC {
        return format!("{ms}ms");
    }
    if ms < MIN {
        return format!("{:.1}s", ms as f64 / SEC as f64);
    }
    if ms < HOUR {
        let (minutes, seconds) = (ms / MIN, (ms % MIN) / SEC);
        return if seconds > 0 {
            format!("{minutes}m{seconds}s")
        } else {
            format!("{minutes}m")
        };
    }
    if ms < DAY {
        let (hours, minutes) = (ms / HOUR, (ms % HOUR) / MIN);
        return if minutes > 0 {
            format!("{hours}h{minutes}m")
        } else {
            format!("{hours}h")
        };
    }
    let (days, hours) = (ms / DAY, (ms % DAY) / HOUR);
    if hours > 0 {
        format!("{days}d{hours}h")
    } else {
        format!("{days}d")
    }
}

/// The shared media query exists only in the browser build; native tests take the
/// full-motion path, which is the one they pin.
#[cfg(target_arch = "wasm32")]
fn reduced_motion() -> bool {
    prefers_reduced_motion()
}

#[cfg(not(target_arch = "wasm32"))]
fn reduced_motion() -> bool {
    false
}

#[component]
pub(crate) fn WorkingRow(label: String) -> Element {
    let started_ms = use_hook(now_ms);
    let mut clock_ms = use_signal(now_ms);
    let reduced = reduced_motion();
    use_future(move || async move {
        loop {
            sleep_ms(if reduced {
                REDUCED_TICK_MS
            } else {
                FRAME_TICK_MS
            })
            .await;
            clock_ms.set(now_ms());
        }
    });
    let elapsed_ms = clock_ms().saturating_sub(started_ms);
    let row_sx = use_hook(|| sx().gap("sm").font_size("sm").color("muted"));
    let duration_sx = use_hook(|| sx().with("font-variant-numeric", "tabular-nums"));
    rsx! {
        Flex { direction: "row", align: "center", sx: row_sx,
            Indicator { color: "primary", processing: !reduced }
            Text { sx: duration_sx, "{format_duration(elapsed_ms)}" }
            Text { aria_hidden: "true", " · " }
            Text { role: "status", "{label}" }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_matches_omp_units() {
        assert_eq!(format_duration(0), "0ms");
        assert_eq!(format_duration(999), "999ms");
        assert_eq!(format_duration(1_500), "1.5s");
        assert_eq!(format_duration(30_000), "30.0s");
        assert_eq!(format_duration(30 * 60_000 + 15_000), "30m15s");
        assert_eq!(format_duration(2 * 3_600_000 + 30 * 60_000), "2h30m");
        assert_eq!(format_duration(3 * 86_400_000 + 2 * 3_600_000), "3d2h");
    }

    #[test]
    fn label_follows_the_most_recent_running_tool() {
        let running = |args: &str| TranscriptItem::Tool {
            id: "t".into(),
            call_id: "c".into(),
            tool_name: "bash".into(),
            args_json: args.into(),
            output: String::new(),
            is_error: false,
            running: true,
            queued: false,
            details_json: String::new(),
            children: Vec::new(),
        };
        let items = vec![
            running(r#"{"i":"Reading config..."}"#),
            running(r#"{"path":"x"}"#),
        ];
        assert_eq!(working_label(&items), IDLE_LABEL);
        let items = vec![
            running(r#"{"path":"x"}"#),
            running(r#"{"i":"Running tests"}"#),
        ];
        assert_eq!(working_label(&items), "Running tests");
        assert_eq!(working_label(&[]), IDLE_LABEL);
    }
}
