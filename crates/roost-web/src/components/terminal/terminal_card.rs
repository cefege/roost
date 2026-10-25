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
use crate::components::ui::{Card, CardContent, CardSize, Checkbox, Icon, IconButton, IconType};
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
        "transform 200ms cubic-bezier(0.05, 0.7, 0.1, 1), opacity 200ms cubic-bezier(0.05, 0.7, 0.1, 1)"
    };
    let style = format!(
        "transform: translateX({dx}px); opacity: {}; transition: {transition};",
        card_swipe_alpha(dx)
    );
    let close_session = session.clone();
    let end_session = session.clone();

    rsx! {
        Card {
            size: CardSize::Sm,
            class: Some(
                "relative h-[204px] min-w-0 cursor-pointer overflow-hidden text-left [-webkit-tap-highlight-color:transparent] focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring data-[active=true]:ring-2 data-[active=true]:ring-primary data-[selected=true]:ring-2 data-[selected=true]:ring-primary"
                    .to_owned(),
            ),
            "data-testid": "terminal-card-{id}",
            "data-active": if active { "true" } else { "false" },
            "data-selected": if selected { "true" } else { "false" },
            role: "button",
            "aria-pressed": selection_mode.then(|| selected.to_string()),
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
            CardContent { class: "flex h-9 shrink-0 items-center gap-2 overflow-hidden pr-11",
                Icon { icon: IconType::Terminal, class: "size-4 shrink-0 text-muted-foreground" }
                div { class: "flex min-w-0 flex-1 flex-col overflow-hidden",
                    span { class: "min-w-0 truncate", "{name}" }
                    if let Some(subtitle) = subtitle {
                        span { class: "block truncate text-muted-foreground",
                            "{subtitle}"
                        }
                    }
                    AgentStatusIndicator { session_id: id.clone() }
                }
            }
            if !selection_mode {
                IconButton {
                    icon: IconType::X,
                    label: "Close terminal session",
                    class: Some("absolute top-0 right-0 size-12".to_owned()),
                    "data-testid": "terminal-card-close-{id}",
                    onclick: move |event: MouseEvent| {
                        event.stop_propagation();
                        event.prevent_default();
                        on_close.call(close_session.clone());
                    },
                }
            }
            if selection_mode {
                Checkbox {
                    class: Some("absolute top-1.5 right-1.5 z-[1] size-6".to_owned()),
                    checked: selected,
                    test_id: format!("terminal-card-check-{id}"),
                    presentational: true,
                }
            }
            // The per-device colour scheme writes --terminal-grid-* inline on
            // <html>; the theme's terminal role would ignore it, and the preview
            // must look like the terminal it previews (as the tab hover card does).
            CardContent { class: "min-h-0 flex-1",
                div { class: "relative h-full overflow-hidden bg-[var(--terminal-grid-bg)]",
                    match preview {
                        Some(rows) if !rows.is_empty() => rsx! {
                            div { class: "block size-full overflow-hidden px-1.5 py-1 font-mono text-xs whitespace-pre box-border text-[var(--terminal-grid-fg)] [&_span]:animate-none!",
                                for (index, row) in rows.iter().enumerate() {
                                    PreviewRow { key: "{index}", row: row.clone() }
                                }
                            }
                        },
                        _ => rsx! {
                            span { class: "absolute top-1/2 left-1/2 -translate-x-1/2 -translate-y-1/2 text-muted-foreground opacity-70",
                                Icon { icon: IconType::Terminal, class: "size-10" }
                            }
                        },
                    }
                }
            }
        }
    }
}

/// One preview row, painted with the renderer's own span styles.
#[component]
fn PreviewRow(row: CellRow) -> Element {
    rsx! {
        div { class: "overflow-hidden whitespace-pre",
            if row.spans.is_empty() {
                "\u{a0}"
            }
            for (index, span) in row.spans.iter().enumerate() {
                span { key: "{index}", style: span_style(span), "{span.text}" }
            }
        }
    }
}
