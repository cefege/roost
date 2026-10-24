//! The one card shown while a terminal pane is opening: a constant eyebrow, a
//! single determinate meter, one friendly step line, and the technical detail
//! collapsed until a step is slow or stuck. Rendered by `CellTerminal`.
//! Entry/exit motion uses the terminal startup keyframes.

use dioxus::prelude::*;
use libero::components::{Box, Paper, ProgressBar, Text, Title};
use libero::sx::sx;

use super::dom::{now_ms, page_visible, sleep_ms};
use super::pane_status::{TerminalStartupNotice, stage_attribute};
use super::startup_overlay_state::StartupOverlayState;
use roost_web_terminal::startup_progress::{StartupChunks, terminal_startup_chunk_detail};

/// The card, or nothing once the journey completed.
#[component]
pub fn TerminalStartupOverlay(notice: Option<TerminalStartupNotice>) -> Element {
    let paper_sx = use_hook(|| {
        sx().width("min(100%, 38ch)")
            .gap("sm")
            .padding("lg")
            .text_align("center")
    });
    let overlay_sx = use_hook(|| {
        sx().position("absolute")
            .with("inset", "0")
            .z_index("5")
            .display("flex")
            .align_items("center")
            .justify_content("center")
            .padding("lg")
            .pointer_events("none")
            .selector(
                "&[data-phase='loading']",
                sx().with("animation", "terminal-startup-in 200ms ease-out both"),
            )
            .selector(
                "&[data-phase='complete']",
                sx().with("animation", "terminal-startup-out 150ms ease-in forwards"),
            )
            .media(
                "(prefers-reduced-motion: reduce)",
                sx().with("animation", "none"),
            )
    });
    let screen_reader_sx = use_hook(|| {
        sx().position("absolute")
            .width("1px")
            .height("1px")
            .padding("0")
            .margin("-1px")
            .overflow("hidden")
            .with("clip", "rect(0, 0, 0, 0)")
            .white_space("nowrap")
            .with("border", "0")
    });
    let details_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .gap("xs")
            .border_top("1px solid")
            .border_color("muted.4")
            .padding_top("sm")
            .font_size("sm")
            .color("muted")
            .selector("&[hidden]", sx().display("none"))
    });
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
        Box {
            component: "div",
            sx: overlay_sx,
            "data-testid": "terminal-loading-status",
            "data-stage": stage_attribute(held.stage),
            "data-session-id": held.session_id.clone(),
            "data-elapsed-seconds": "{elapsed}",
            "data-percent": "{percent}",
            "data-phase": if state.finishing() { "complete" } else { "loading" },
            Box {
                component: "div",
                sx: screen_reader_sx,
                role: "status",
                "aria-live": "polite",
                "aria-atomic": "true",
                "{title}. {detail}"
            }
            Paper {
                sx: paper_sx,
                Text { size: "sm", color: "text-dimmed", "data-testid": "terminal-loading-title", "Opening terminal" }
                Title { component: "h2", size: "lg", "data-testid": "terminal-loading-percent", "{percent}%" }
                ProgressBar {
                    value: state.percent(),
                    max: 100.0,
                    aria_label: "Terminal startup progress",
                    "data-testid": "terminal-loading-progress",
                }
                Text { size: "sm", color: "text-dimmed", "data-testid": "terminal-loading-detail", "{step_label}" }
                Box {
                    component: "div",
                    sx: details_sx,
                    "data-testid": "terminal-loading-details",
                    hidden: !state.slow(),
                    div { "data-testid": "terminal-loading-technical", "{held.detail}" }
                    if let Some(part) = chunk_detail {
                        div { "data-testid": "terminal-loading-progress-label", "{part}" }
                    }
                    div {
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
