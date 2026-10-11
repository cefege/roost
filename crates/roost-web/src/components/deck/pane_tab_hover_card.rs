//! The desktop hover card for a terminal tab: title and agent chip, program
//! subtitle, direct-carrier chip and short cwd, anchored under the tab.
//! `PaneStrip` decides the dwell and which tab; this renders it. Ports
//! `apps/web/src/components/deck/PaneTabHoverCard.tsx` without its live
//! preview, whose renderer (`renderer/terminalPreview.ts`) is not ported, so
//! the preview well renders empty exactly as v2 does when no preview exists.

use dioxus::prelude::*;
use roost_client_core::store::selectors::session_by_id;
use roost_client_core::store::terminal_transport::session_terminal_transport_kind;

use super::deck_dom::{ClientBox, DeckContainer};
use super::inline_style::px;
use super::pane_tab::direct_transport_label;
use crate::components::agents::agent_status_indicator::AgentStatusIndicator;
use crate::components::md::surface::SurfaceRadius;
use crate::components::md::{Icon, IconSize, Surface};
use crate::platform::worker_paths::short_worker_path;
use crate::pump::use_store;
use crate::session_naming::{program_subtitle, session_title};

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
        "max(var(--md-space-2), min({}, calc({room} - var(--workbench-tab-hovercard-width) - var(--md-space-2))))",
        px(anchor.left - origin.left)
    );
    let top = format!(
        "calc({} + var(--md-space-2))",
        px(anchor.bottom() - origin.top)
    );
    rsx! {
        Surface {
            level: 3,
            elevation: 3,
            radius: SurfaceRadius::Md,
            class: "df-tab-hovercard",
            test_id: "tab-hovercard",
            style: format!("left: {left}; top: {top};"),
            div { class: "df-tab-hovercard-head",
                Icon { name: "terminal", size: IconSize::Sm }
                span { class: "df-tab-hovercard-title", "{reading.title}" }
                AgentStatusIndicator { session_id: session_id.clone() }
            }
            if let Some(subtitle) = reading.subtitle {
                div { class: "df-tab-hovercard-line", "{subtitle}" }
            }
            if let Some(direct) = reading.direct {
                div { class: "df-tab-hovercard-chip", title: direct,
                    Icon { name: "bolt", size: IconSize::Sm }
                    "{direct}"
                }
            }
            div { class: "df-tab-hovercard-cwd", "{reading.cwd}" }
            div { class: "df-tab-hovercard-preview", "data-preview": "false",
                div { class: "terminal-card-preview-text" }
            }
        }
    }
}
