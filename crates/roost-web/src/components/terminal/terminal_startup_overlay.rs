//! The one card shown while a terminal pane is opening: a constant eyebrow, a
//! single determinate meter, one friendly step line, and the technical detail
//! collapsed until a step is slow or stuck. Rendered by `CellTerminal`; its
//! clock is `startup_overlay_state`. Ports the component of
//! `apps/web/src/components/terminal/TerminalStartupOverlay.tsx` and attaches
//! its byte-for-byte copied `TerminalStartupOverlay.css`.

use dioxus::prelude::*;
use roost_web_terminal::startup_progress::{StartupChunks, terminal_startup_chunk_detail};

use super::dom::{now_ms, page_visible, sleep_ms};
use super::pane_status::{TerminalStartupNotice, stage_attribute};
use super::startup_overlay_state::StartupOverlayState;
use crate::components::md::stylesheet::use_md_stylesheet;
use crate::components::md::{Surface, SurfaceRadius};

/// The card's stylesheet, served from `assets/components/terminal/`.
pub const STARTUP_OVERLAY_STYLESHEET_HREF: &str = "/components/terminal/TerminalStartupOverlay.css";

/// The card, or nothing once the journey completed.
#[component]
pub fn TerminalStartupOverlay(notice: Option<TerminalStartupNotice>) -> Element {
    use_md_stylesheet(STARTUP_OVERLAY_STYLESHEET_HREF);
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
    let fill = format!("width: {}%;", state.percent());
    let elapsed = state.elapsed_seconds();
    rsx! {
        div {
            class: "terminal-startup",
            "data-testid": "terminal-loading-status",
            "data-stage": stage_attribute(held.stage),
            "data-session-id": held.session_id.clone(),
            "data-elapsed-seconds": "{elapsed}",
            "data-percent": "{percent}",
            "data-phase": if state.finishing() { "complete" } else { "loading" },
            div {
                class: "terminal-startup__announce",
                role: "status",
                "aria-live": "polite",
                "aria-atomic": "true",
                "{title}. {detail}"
            }
            Surface {
                level: 1,
                elevation: 2,
                radius: SurfaceRadius::Lg,
                pad: 6,
                border: true,
                class: "terminal-startup__card",
                div {
                    class: "terminal-startup__eyebrow md-label-m",
                    "data-testid": "terminal-loading-title",
                    "Opening terminal"
                }
                div {
                    class: "terminal-startup__percent md-headline-s",
                    "data-testid": "terminal-loading-percent",
                    "{percent}%"
                }
                div {
                    class: "terminal-startup__track",
                    "data-testid": "terminal-loading-progress",
                    role: "progressbar",
                    "aria-valuemin": "0",
                    "aria-valuemax": "100",
                    "aria-valuenow": "{percent}",
                    div {
                        class: "terminal-startup__fill",
                        "data-testid": "terminal-loading-progress-fill",
                        style: fill,
                    }
                }
                div {
                    class: "terminal-startup__step md-body-s",
                    "data-testid": "terminal-loading-detail",
                    "{step_label}"
                }
                div {
                    class: "terminal-startup__details",
                    "data-testid": "terminal-loading-details",
                    hidden: !state.slow(),
                    div { class: "md-body-s", "data-testid": "terminal-loading-technical", "{held.detail}" }
                    if let Some(part) = chunk_detail {
                        div { class: "md-label-m", "data-testid": "terminal-loading-progress-label", "{part}" }
                    }
                    div {
                        class: "md-label-m",
                        "data-testid": "terminal-loading-elapsed",
                        "aria-hidden": "true",
                        "This step has taken {elapsed}s"
                    }
                    if let Some(reason) = held.stuck_reason.clone() {
                        div { class: "md-body-s", "data-testid": "terminal-loading-stuck-reason", "{reason}" }
                    }
                }
            }
        }
    }
}
