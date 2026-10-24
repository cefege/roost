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
use super::pane_registry::PaneRegistry;
use crate::components::agents::agent_status_indicator::AgentStatusIndicator;
use crate::components::deck::deck_swipe::card_swipe_alpha;
use crate::pump::Pump;
use crate::session_naming::{program_subtitle, session_title};
use libero::components::{ActionIcon, Box, Checkbox, Paper, Text};
use libero::sx::sx;

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
    pump: Pump,
    pane_registry: PaneRegistry,
) -> Element {
    use_context_provider(|| pump.clone());
    use_context_provider(|| pane_registry.clone());
    let revision = pump.revision();
    let _revision = revision();
    let panes = pane_registry;
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
    let paper_sx = use_hook(|| {
        sx().position("relative")
            .min_width("0")
            .overflow("hidden")
            .text_align("start")
    });
    let header_sx = use_hook(|| {
        sx().display("flex")
            .height("2.25rem")
            .flex_shrink("0")
            .align_items("center")
            .gap("sm")
            .overflow("hidden")
            .padding_inline_start("sm")
            .padding_inline_end("2.75rem")
    });
    let glyph_frame_sx = use_hook(|| {
        sx().display("flex")
            .width("1.5rem")
            .height("1.5rem")
            .flex_shrink("0")
            .align_items("center")
            .justify_content("center")
            .border_radius("xs")
            .background("neutral.1")
    });
    let glyph_sx = use_hook(|| {
        sx().font_family("var(--lsx-code-font-family)")
            .font_weight("bold")
    });
    let title_sx = use_hook(|| {
        sx().min_width("0")
            .overflow("hidden")
            .with("overscroll-behavior-y", "auto")
            .text_overflow("ellipsis")
            .white_space("nowrap")
            .font_size("sm")
            .font_weight("500")
    });
    let subtitle_sx = use_hook(|| {
        sx().display("block")
            .overflow("hidden")
            .with("overscroll-behavior-y", "auto")
            .text_overflow("ellipsis")
            .white_space("nowrap")
            .font_size("xs")
            .color("muted")
    });
    let preview_frame_sx = use_hook(|| {
        sx().position("relative")
            .margin("xs")
            .height("10rem")
            .overflow("hidden")
            .with("border-radius", "12px 12px 20px 20px")
            .background("var(--terminal-grid-bg)")
    });
    let preview_sx = use_hook(|| {
        sx().display("block")
            .width("100%")
            .height("100%")
            .overflow("hidden")
            .with("overscroll-behavior-y", "auto")
            .border_radius("inherit")
            .padding_inline("xs")
            .padding_block("xs")
            .font_family("var(--lsx-code-font-family)")
            .with("font-size", "var(--term-preview-font-size)")
            .with("line-height", "var(--term-preview-line-height)")
            .white_space("pre")
            .box_sizing("border-box")
            .color("var(--terminal-grid-fg)")
            .selector("& span", sx().with("animation", "none"))
    });
    let preview_empty_sx = use_hook(|| {
        sx().position("absolute")
            .with("top", "50%")
            .with("left", "50%")
            .transform("translate(-50%, -50%)")
            .font_size("xxl")
            .with("line-height", "1")
            .color("muted")
            .opacity("0.7")
    });
    let meta_sx = use_hook(|| {
        sx().display("flex")
            .min_width("0")
            .flex("1")
            .flex_direction("column")
            .overflow("hidden")
    });
    let close_session = session.clone();
    let end_session = session.clone();

    rsx! {
        Paper {
            sx: paper_sx,
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
            Box { component: "div", sx: header_sx,
                Box { component: "span", sx: glyph_frame_sx, Box { component: "span", sx: glyph_sx.clone(), "$" } }
                Box { component: "div", sx: meta_sx,
                    Text { component: "span", sx: title_sx, "{name}" }
                    if let Some(subtitle) = subtitle {
                        Text { component: "span", sx: subtitle_sx, "{subtitle}" }
                    }
                    AgentStatusIndicator { session_id: id.clone() }
                }
            }
            if !selection_mode {
                ActionIcon {
                    icon: pictogram_icons_lucide::x::outlined,
                    aria_label: "Close terminal session",
                    size: "lg",
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
                    checked: selected,
                    aria_label: "Select terminal",
                    onclick: move |event: MouseEvent| event.stop_propagation(),
                    onchange: move |_| on_toggle_select.call(id.clone()),
                }
            }
            // The per-device colour scheme writes --terminal-grid-* inline on
            // <html>; the theme's terminal role would ignore it, and the preview
            // must look like the terminal it previews (as the tab hover card does).
            Box { component: "div", sx: preview_frame_sx,
                match preview {
                    Some(rows) if !rows.is_empty() => rsx! {
                        Box { component: "div", sx: preview_sx,
                            for (index, row) in rows.iter().enumerate() {
                                PreviewRow { key: "{index}", row: row.clone() }
                            }
                        }
                    },
                    _ => rsx! {
                        Box { component: "span", sx: preview_empty_sx,
                            Box { component: "span", sx: glyph_sx, "$" }
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
    let row_sx = use_hook(|| sx().overflow("hidden").white_space("pre"));
    rsx! {
        Box { component: "div", sx: row_sx,
            if row.spans.is_empty() {
                "\u{a0}"
            }
            for (index, span) in row.spans.iter().enumerate() {
                span { key: "{index}", style: span_style(span), "{span.text}" }
            }
        }
    }
}
