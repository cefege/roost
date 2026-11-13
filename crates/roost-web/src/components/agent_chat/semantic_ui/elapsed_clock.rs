//! Semantic timers read retained origins from the accepted conversation replica.
//! One owned browser cadence drives visible running counters without changing OMP data.
//! Hidden or remounted views recover elapsed time from the same monotonic clock.
use dioxus::prelude::*;
use roost_client_core::{
    Clock,
    client::agent_chat::{OmpTimerKind, OmpTimerReading},
};

use super::media::MediaContext;
use crate::{platform::clock::BrowserClock, pump::use_store};

#[derive(Clone, Copy)]
struct PresentationClock {
    now_ms: Signal<u64>,
}

pub(super) fn use_clock() {
    let clock = use_hook(BrowserClock::new);
    let mut now_ms = use_signal(|| clock.now_ms());
    use_context_provider(|| PresentationClock { now_ms });
    use_future(move || {
        let clock = clock.clone();
        async move {
            loop {
                crate::components::terminal::dom::sleep_ms(100).await;
                now_ms.set(clock.now_ms());
            }
        }
    });
}

#[component]
pub(super) fn NativeElapsed(
    surface: String,
    node: String,
    #[props(default = OmpTimerKind::Elapsed)] kind: OmpTimerKind,
    #[props(default = "short".to_owned())] format: String,
    #[props(default)] fact: Option<(String, String)>,
) -> Element {
    let clock = use_context::<PresentationClock>();
    let media = use_context::<Signal<MediaContext>>();
    let pump = use_store();
    let reading = {
        let media = media.read();
        let core = pump.core();
        let core = core.borrow();
        let now_ms = *clock.now_ms.peek();
        core.store()
            .agent_chat
            .transcripts
            .get(&media.conversation_id)
            .and_then(|loaded| match &fact {
                Some((item, column)) => loaded
                    .presentation_clocks
                    .fact(&surface, &node, item, column, now_ms),
                None => loaded
                    .presentation_clocks
                    .timer(&surface, &node, kind, now_ms),
            })
    };
    let Some(reading) = reading else {
        return rsx! { span { role: "alert", class: "text-destructive", "OMP timer unavailable" } };
    };
    if reading.running {
        let _tick = (clock.now_ms)();
    }
    let label = timer_label(reading, &format, kind);
    let state = if reading.countdown {
        "countdown"
    } else {
        "elapsed"
    };
    rsx! {
        time {
            "data-omp-timer": state,
            "data-omp-timer-surface": surface,
            "data-omp-timer-node": node,
            "{label}"
            if reading.countdown {
                span { class: "sr-only", " remaining" }
            } else if reading.running {
                span { class: "sr-only", " elapsed and counting" }
            }
        }
    }
}

/// Prop-tree timers (hovercards) carry no replica node id; they run from this mount.
#[component]
pub(super) fn PropElapsed(
    age: f64,
    stopped: Option<f64>,
    #[props(default = "short".to_owned())] format: String,
) -> Element {
    let clock = try_use_context::<PresentationClock>();
    let mounted_ms = use_hook(|| clock.map(|clock| *clock.now_ms.peek()).unwrap_or_default());
    let now_ms = match (clock, stopped) {
        (Some(clock), None) => (clock.now_ms)(),
        _ => mounted_ms,
    };
    let reading =
        OmpTimerReading::from_elapsed_props(age, stopped, now_ms.saturating_sub(mounted_ms));
    let label = timer_label(reading, &format, OmpTimerKind::Elapsed);
    let state = if reading.countdown {
        "countdown"
    } else {
        "elapsed"
    };
    rsx! {
        time {
            "data-omp-timer": state,
            "{label}"
            if reading.countdown {
                span { class: "sr-only", " remaining" }
            } else if reading.running {
                span { class: "sr-only", " elapsed and counting" }
            }
        }
    }
}

fn timer_label(reading: OmpTimerReading, format: &str, kind: OmpTimerKind) -> String {
    if kind == OmpTimerKind::AgentRetry {
        return if reading.milliseconds < 0.0 {
            format!("in {}", super::content::duration(-reading.milliseconds))
        } else {
            "now".to_owned()
        };
    }
    let milliseconds = if reading.countdown {
        -reading.milliseconds
    } else {
        reading.milliseconds
    };
    if format == "clock" {
        let seconds = if reading.countdown {
            (milliseconds.max(0.0) / 1000.0).ceil() as u64
        } else {
            (milliseconds.max(0.0) / 1000.0).floor() as u64
        };
        return format!(
            "{:02}:{:02}:{:02}",
            seconds / 3600,
            (seconds / 60) % 60,
            seconds % 60
        );
    }
    if reading.countdown {
        format!("{}s", (milliseconds.max(0.0) / 1000.0).ceil() as u64)
    } else {
        super::content::duration(milliseconds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(milliseconds: f64, countdown: bool) -> OmpTimerReading {
        OmpTimerReading {
            milliseconds,
            countdown,
            running: true,
        }
    }

    #[test]
    fn cached_countdown_uses_the_tui_whole_remaining_seconds_and_holds_zero() {
        assert_eq!(
            timer_label(reading(-2500.0, true), "short", OmpTimerKind::Elapsed),
            "3s"
        );
        assert_eq!(
            timer_label(reading(-2000.0, true), "short", OmpTimerKind::Elapsed),
            "2s"
        );
        assert_eq!(
            timer_label(reading(0.0, true), "short", OmpTimerKind::Elapsed),
            "0s"
        );
        assert_eq!(
            timer_label(reading(-2500.0, true), "clock", OmpTimerKind::Elapsed),
            "00:00:03"
        );
    }

    #[test]
    fn ordinary_elapsed_and_retry_keep_the_source_duration_boundaries() {
        assert_eq!(
            timer_label(reading(59999.0, false), "short", OmpTimerKind::Elapsed),
            "1m"
        );
        assert_eq!(
            timer_label(reading(59999.0, false), "clock", OmpTimerKind::Elapsed),
            "00:00:59"
        );
        assert_eq!(
            timer_label(reading(-990.0, true), "short", OmpTimerKind::AgentRetry),
            "in 990ms"
        );
        assert_eq!(
            timer_label(reading(-1110.0, true), "short", OmpTimerKind::AgentRetry),
            "in 1.1s"
        );
        assert_eq!(
            timer_label(reading(0.0, true), "short", OmpTimerKind::AgentRetry),
            "now"
        );
    }
}
