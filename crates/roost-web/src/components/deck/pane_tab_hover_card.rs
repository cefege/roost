//! The desktop hover card for a terminal tab: title and agent chip, program
//! subtitle, direct-carrier chip and short cwd, anchored under the tab.
//! `PaneStrip` decides the dwell and which tab; this renders it. Ports
//! `apps/web/src/components/deck/PaneTabHoverCard.tsx` without its live
//! preview, whose renderer (`renderer/terminalPreview.ts`) is not ported, so
//! the preview well renders empty exactly as v2 does when no preview exists.

use dioxus::prelude::*;
use libero::components::Pictogram;
use roost_client_core::store::selectors::session_by_id;
use roost_client_core::store::terminal_transport::session_terminal_transport_kind;

use super::deck_dom::{ClientBox, DeckContainer};
use super::inline_style::px;
use super::pane_tab::direct_transport_label;
use crate::components::agents::agent_status_indicator::AgentStatusIndicator;
use crate::platform::worker_paths::short_worker_path;
use crate::pump::use_store;
use crate::session_naming::{program_subtitle, session_title};
use libero::components::{Box, Paper, Text};
use libero::sx::sx;

/// What the card shows, read in one borrow.
#[derive(Debug, Clone, PartialEq)]
struct HoverCardReading {
    title: String,
    subtitle: Option<String>,
    direct: Option<&'static str>,
    cwd: String,
}

/// The card for `session_id`, under `anchor`.
#[component]
pub fn PaneTabHoverCard(session_id: String, anchor: ClientBox) -> Element {
    let pump = use_store();
    let card_sx = use_hook(|| {
        sx().position("fixed")
            .with("z-index", "70")
            .pointer_events("none")
            .display("flex")
            .flex_direction("column")
            .width("var(--workbench-tab-hovercard-width)")
            .font_size("sm")
            .gap("sm")
            .padding("md")
            .transition("opacity 150ms")
            .media("(prefers-reduced-motion: reduce)", sx().transition("none"))
    });
    let heading_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("row")
            .align_items("center")
            .gap("sm")
            .min_width("0")
    });
    let title_sx = use_hook(|| {
        sx().flex("1")
            .min_width("0")
            .overflow("hidden")
            .with("overscroll-behavior-y", "auto")
            .text_overflow("ellipsis")
            .white_space("nowrap")
            .font_weight("500")
    });
    let subtitle_sx = use_hook(|| {
        sx().overflow("hidden")
            .with("overscroll-behavior-y", "auto")
            .text_overflow("ellipsis")
            .white_space("nowrap")
            .color("muted")
    });
    let direct_sx = use_hook(|| {
        sx().display("inline-flex")
            .flex_direction("row")
            .align_items("center")
            .gap("xs")
            .align_self("flex-start")
            .color("warning")
    });
    let cwd_sx = use_hook(|| {
        sx().overflow("hidden")
            .with("overscroll-behavior-y", "auto")
            .text_overflow("ellipsis")
            .white_space("nowrap")
            .font_size("xs")
            .font_family("monospace")
            .color("muted")
    });
    let preview_sx = use_hook(|| {
        sx().margin_top("xs")
            .height("6rem")
            .overflow("hidden")
            .border_radius("md")
            .background("var(--terminal-grid-bg)")
    });
    let preview_content_sx = use_hook(|| {
        sx().display("block")
            .width("100%")
            .height("100%")
            .overflow("hidden")
            .with("overscroll-behavior-y", "auto")
            .border_radius("inherit")
            .padding_inline("xs")
            .padding_block("xs")
            .font_family("monospace")
            .white_space("pre")
            .box_sizing("border-box")
            .color("var(--terminal-grid-fg)")
            .selector("& span", sx().animation("none"))
    });
    let container = try_use_context::<DeckContainer>();
    let reading = {
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        session_by_id(store, &session_id).map(|session| {
            let worker_os = store
                .workers
                .get(session.worker_fp.as_str())
                .map(|worker| worker.os.as_str());
            HoverCardReading {
                title: session_title(store, session),
                subtitle: program_subtitle(store, session),
                direct: direct_transport_label(session_terminal_transport_kind(store, &session_id)),
                cwd: short_worker_path(worker_os, &session.cwd),
            }
        })
    };
    let Some(reading) = reading else {
        return rsx! {};
    };
    // The deck's transform makes it the containing block of this fixed card,
    // so the tab's viewport box is moved into the deck's coordinates and the
    // right-edge clamp is the deck's width, not the viewport's.
    let origin = container
        .map(|container| container.origin())
        .filter(|origin| origin.width > 0.0)
        .unwrap_or_default();
    let room = if origin.width > 0.0 {
        px(origin.width)
    } else {
        "100vw".to_owned()
    };
    let left = format!(
        "max(8px, min({}, calc({room} - var(--workbench-tab-hovercard-width) - 8px)))",
        px(anchor.left - origin.left)
    );
    let top = format!("calc({} + 8px)", px(anchor.bottom() - origin.top));
    rsx! {
        Paper {
            shadow: "md",
            sx: card_sx,
            "data-testid": "tab-hovercard",
            style: format!("left: {left}; top: {top};"),
            Box { component: "div", sx: heading_sx,
                Pictogram { icon: pictogram_icons_lucide::terminal::outlined, width: "1rem", height: "1rem" }
                Text { component: "span", sx: title_sx, "{reading.title}" }
                AgentStatusIndicator { session_id: session_id.clone() }
            }
            if let Some(subtitle) = reading.subtitle {
                Box { component: "div", sx: subtitle_sx, "{subtitle}" }
            }
            if let Some(direct) = reading.direct {
                Box { component: "div", sx: direct_sx, title: direct,
                    Pictogram { icon: pictogram_icons_lucide::zap::outlined, width: "1rem", height: "1rem" }
                    "{direct}"
                }
            }
            Box { component: "div", sx: cwd_sx, "{reading.cwd}" }
            // The colour scheme writes its palette inline on <html>; bg-terminal reads the theme's fixed terminal colour and would ignore it.
            Box { component: "div", sx: preview_sx, "data-preview": "false",
                Box { component: "div", sx: preview_content_sx, }
            }
        }
    }
}
