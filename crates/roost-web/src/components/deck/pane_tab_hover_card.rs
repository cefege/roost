//! The tab controller's stock HoverCard presentation: title, program,
//! carrier and cwd. `PaneStrip` owns dwell and dismissal; this view keeps
//! the anchor in the transformed deck's coordinate space.

use dioxus::prelude::*;
use roost_client_core::store::selectors::session_by_id;

use super::deck_dom::{ClientBox, DeckContainer};
use super::inline_style::px;
use crate::components::agents::agent_status_indicator::AgentStatusIndicator;
use crate::components::ui::{HoverCardPresentation, Icon, IconType};
use crate::platform::worker_paths::short_worker_path;
use crate::pump::use_store;
use crate::session_naming::session_title;

/// What the card shows, read in one borrow.
#[derive(Debug, Clone, PartialEq)]
struct HoverCardReading {
    title: String,
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
        HoverCardPresentation {
            class: "fixed z-[70] pointer-events-none w-[var(--workbench-tab-hovercard-width)] starting:opacity-0 transition-opacity duration-150 motion-reduce:transition-none",
            "data-testid": "tab-hovercard",
            role: "tooltip",
            style: format!("left: {left}; top: {top};"),
            div { class: "flex flex-col gap-1",
                div { class: "flex items-center gap-2 min-w-0 text-sm",
                    Icon { icon: IconType::Terminal, class: "size-4 shrink-0 text-muted-foreground" }
                    span { class: "flex-1 min-w-0 truncate font-medium [font-family:var(--term-font-family)]", "{reading.title}" }
                    AgentStatusIndicator { session_id: session_id.clone() }
                }
                div { class: "truncate text-xs font-mono text-muted-foreground", "{reading.cwd}" }
            }
        }
    }
}
