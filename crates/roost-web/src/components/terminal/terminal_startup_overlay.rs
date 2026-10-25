//! The one card shown while a terminal pane is opening: a constant eyebrow, a
//! single determinate meter, one friendly step line, and the technical detail
//! collapsed until a step is slow or stuck. Rendered by `CellTerminal`; its
//! entry/exit keyframes are Tailwind utilities (`animate-terminal-startup-*`).

use crate::components::ui::{Card, CardContent, Progress};
use dioxus::prelude::*;
use roost_web_terminal::startup_progress::{StartupChunks, terminal_startup_chunk_detail};

use super::dom::{now_ms, page_visible, sleep_ms};
use super::pane_status::{TerminalStartupNotice, stage_attribute};
use super::startup_overlay_state::StartupOverlayState;

/// The card, or nothing once the journey completed.
#[component]
pub fn TerminalStartupOverlay(notice: Option<TerminalStartupNotice>) -> Element {
    let mut clock = use_signal(StartupOverlayState::default);
    use_effect(use_reactive((&notice,), move |(notice,)| {
        clock.write().set_notice(notice, now_ms());
    }));
    use_future(move || async move {
        loop {
            let now = now_ms();
            let due = clock.peek().next_tick_ms(now).unwrap_or(now + 200);
            sleep_ms(due.saturating_sub(now).max(16)).await;
            let visible = page_visible();
            let mut next = clock.peek().clone();
            next.tick(now_ms(), visible);
            if *clock.peek() != next {
                clock.set(next);
            }
        }
    });

    let state = clock.read();
    let Some(held) = state.held() else {
        return rsx! {};
    };
    let percent = state.percent().round();
    let (title, detail) = state.announcement();
    let step_label = held.stage.step().label;
    let chunk_detail = terminal_startup_chunk_detail(
        held.progress
            .map(|(received, total)| StartupChunks { received, total }),
    );
    let elapsed = state.elapsed_seconds();
    rsx! {
        div {
            class: if state.finishing() {
                "absolute inset-0 z-[5] flex items-center justify-center p-6 pointer-events-none animate-terminal-startup-out motion-reduce:animate-none"
            } else {
                "absolute inset-0 z-[5] flex items-center justify-center p-6 pointer-events-none animate-terminal-startup-in motion-reduce:animate-none"
            },
            "data-testid": "terminal-loading-status",
            "data-stage": stage_attribute(held.stage),
            "data-session-id": held.session_id.clone(),
            "data-elapsed-seconds": "{elapsed}",
            "data-percent": "{percent}",
            "data-phase": if state.finishing() { "complete" } else { "loading" },
            div {
                class: "sr-only",
                role: "status",
                "aria-live": "polite",
                "aria-atomic": "true",
                "{title}. {detail}"
            }
            Card {
                class: Some("w-[min(100%,38ch)] text-center".to_owned()),
                CardContent {
                    div {
                        class: "font-medium text-muted-foreground",
                        "data-testid": "terminal-loading-title",
                        "Opening terminal"
                    }
                    div {
                        class: "tabular-nums",
                        "data-testid": "terminal-loading-percent",
                        "{percent}%"
                    }
                    Progress {
                        value: Some(state.percent() / 100.0),
                        label: "Terminal startup progress",
                        test_id: "terminal-loading-progress",
                        fill_test_id: "terminal-loading-progress-fill",
                    }
                    div {
                        class: "text-muted-foreground",
                        "data-testid": "terminal-loading-detail",
                        "{step_label}"
                    }
                    div {
                        class: "flex flex-col gap-1 border-t border-border pt-3 text-muted-foreground [&[hidden]]:hidden",
                        "data-testid": "terminal-loading-details",
                        hidden: !state.slow(),
                        div { "data-testid": "terminal-loading-technical", "{held.detail}" }
                        if let Some(part) = chunk_detail {
                            div { class: "font-medium", "data-testid": "terminal-loading-progress-label", "{part}" }
                        }
                        div {
                            class: "font-medium",
                            "data-testid": "terminal-loading-elapsed",
                            "aria-hidden": "true",
                            "This step has taken {elapsed}s"
                        }
                        if let Some(reason) = held.stuck_reason.clone() {
                            div { "data-testid": "terminal-loading-stuck-reason", "{reason}" }
                        }
                    }
                }
            }
        }
    }
}
