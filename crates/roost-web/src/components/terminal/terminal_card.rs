//! One terminal card in the mobile terminal grid: a header (glyph, title,
//! subtitle, agent status, close) over a preview of the terminal's newest
//! painted rows, with swipe-to-close. Rendered by DECK's mobile tab sheet.
//! Ports `apps/web/src/components/terminal/TerminalCard.tsx` and the
//! `renderPreview` of `apps/web/src/renderer/terminalPreview.ts`.

use dioxus::prelude::*;
use roost_client_core::store::Session;
use roost_protocol::cell::CellRow;
use roost_web_terminal::cell_row::span_style;

use super::card_swipe::CardSwipe;
use super::dom::{now_ms, sleep_ms};
use super::pane_registry::use_pane_registry;
use crate::components::agents::agent_status_indicator::AgentStatusIndicator;
use crate::components::deck::deck_swipe::card_swipe_alpha;
use crate::pump::use_store;
use crate::session_naming::{program_subtitle, session_title};

/// How long a dismissed card slides before it closes.
const DISMISS_SLIDE_MS: u64 = 180;

/// The card. Props are v2's, snake-cased.
#[component]
#[allow(clippy::too_many_arguments)]
pub fn TerminalCard(
    session: Session,
    active: bool,
    on_select: EventHandler<String>,
    on_close: EventHandler<Session>,
    on_close_sheet: EventHandler<()>,
    selection_mode: bool,
    selected: bool,
    on_toggle_select: EventHandler<String>,
) -> Element {
    let pump = use_store();
    let panes = use_pane_registry();
    let (name, subtitle) = {
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        let subtitle = session
            .git_branch
            .clone()
            .or_else(|| program_subtitle(store, &session));
        (session_title(store, &session), subtitle)
    };
    // Taken once, when the card mounts (the sheet opens), like v2.
    let preview = use_hook(|| panes.preview_rows(session.id.as_str()));
    let mut swipe = use_signal(CardSwipe::default);
    let mut swiping = use_signal(|| false);
    let id = session.id.as_str().to_owned();

    let activate = {
        let id = id.clone();
        move || {
            if selection_mode {
                on_toggle_select.call(id.clone());
                return;
            }
            if swipe.write().take_swiped() {
                return;
            }
            on_select.call(id.clone());
            on_close_sheet.call(());
        }
    };
    let mut on_click = activate.clone();
    let mut on_key = activate;
    let dx = swipe.read().dx();
    let transition = if swiping() {
        "none"
    } else {
        "transform var(--md-sys-motion-duration-short4, 200ms) var(--md-sys-motion-easing-emphasized-decelerate, cubic-bezier(0.05, 0.7, 0.1, 1)), opacity var(--md-sys-motion-duration-short4, 200ms) var(--md-sys-motion-easing-emphasized-decelerate, cubic-bezier(0.05, 0.7, 0.1, 1))"
    };
    let style = format!(
        "transform: translateX({dx}px); opacity: {}; transition: {transition};",
        card_swipe_alpha(dx)
    );
    let close_session = session.clone();
    let end_session = session.clone();

    rsx! {
        div {
            class: "terminal-card",
            "data-testid": "terminal-card-{id}",
            "data-active": if active { "true" } else { "false" },
            "data-selected": if selected { "true" } else { "false" },
            role: "button",
            tabindex: "0",
            title: "{name}",
            style,
            onclick: move |_| on_click(),
            ontouchstart: move |event: TouchEvent| {
                if selection_mode {
                    return;
                }
                let Some(touch) = event.touches().into_iter().next() else { return };
                let point = touch.client_coordinates();
                swipe.write().start(point.x, point.y, now_ms() as f64);
                swiping.set(true);
            },
            ontouchmove: move |event: TouchEvent| {
                let Some(touch) = event.touches().into_iter().next() else { return };
                let point = touch.client_coordinates();
                let moved = swipe.write().move_to(point.x, point.y, now_ms() as f64);
                if moved.is_some() {
                    event.prevent_default();
                }
            },
            ontouchend: move |_| {
                swiping.set(false);
                if swipe.write().end() {
                    let session = end_session.clone();
                    spawn(async move {
                        sleep_ms(DISMISS_SLIDE_MS).await;
                        on_close.call(session);
                    });
                }
            },
            onkeydown: move |event: KeyboardEvent| {
                let key = event.key().to_string();
                if key == "Enter" || key == " " {
                    event.prevent_default();
                    on_key();
                }
            },
            div { class: "terminal-card-header",
                span { class: "terminal-card-favicon",
                    span { class: "terminal-card-glyph", "$" }
                }
                div { class: "terminal-card-title-wrap",
                    span { class: "terminal-card-title", "{name}" }
                    if let Some(subtitle) = subtitle {
                        span { class: "terminal-card-subtitle", "{subtitle}" }
                    }
                    AgentStatusIndicator { session_id: id.clone() }
                }
            }
            if !selection_mode {
                button {
                    r#type: "button",
                    class: "terminal-card-close",
                    "data-testid": "terminal-card-close-{id}",
                    "aria-label": "Close terminal session",
                    onclick: move |event| {
                        event.stop_propagation();
                        event.prevent_default();
                        on_close.call(close_session.clone());
                    },
                    CloseGlyph {}
                }
            }
            if selection_mode {
                span {
                    class: "terminal-card-check",
                    "data-testid": "terminal-card-check-{id}",
                    "data-checked": if selected { "true" } else { "false" },
                    CheckGlyph {}
                }
            }
            div { class: "terminal-card-preview",
                match preview {
                    Some(rows) if !rows.is_empty() => rsx! {
                        div { class: "terminal-card-preview-text", style: "display: block;",
                            for (index, row) in rows.iter().enumerate() {
                                PreviewRow { key: "{index}", row: row.clone() }
                            }
                        }
                    },
                    _ => rsx! {
                        span { class: "terminal-card-preview-glyph", style: "color: var(--text-lo);",
                            span { class: "terminal-card-glyph", "$" }
                        }
                    },
                }
            }
        }
    }
}

/// One preview row, painted with the renderer's own span styles.
#[component]
fn PreviewRow(row: CellRow) -> Element {
    rsx! {
        div { class: "terminal-card-preview-row",
            if row.spans.is_empty() {
                "\u{a0}"
            }
            for (index, span) in row.spans.iter().enumerate() {
                span { key: "{index}", style: span_style(span), "{span.text}" }
            }
        }
    }
}

#[component]
fn CloseGlyph() -> Element {
    rsx! {
        svg { width: "18", height: "18", view_box: "0 0 24 24", fill: "none", stroke: "currentColor",
            stroke_width: "2", stroke_linecap: "round", "aria-hidden": "true",
            path { d: "M18 6 6 18M6 6l12 12" }
        }
    }
}

#[component]
fn CheckGlyph() -> Element {
    rsx! {
        svg { width: "14", height: "14", view_box: "0 0 24 24", fill: "none", stroke: "currentColor",
            stroke_width: "3", stroke_linecap: "round", stroke_linejoin: "round", "aria-hidden": "true",
            path { d: "M20 6 9 17l-5-5" }
        }
    }
}
