//! The compact top bar: the drawer control and the current route's title, shown
//! above the editor on a phone. Ported from
//! `apps/web/src/components/layout/MobileTopBar.tsx`.
//!
//! It is a bar in the editor's flex column, not a fixed overlay. A floating bar
//! would occlude the input line, and the input line is the one row of a terminal
//! a reader cannot afford to lose behind chrome.

use dioxus::prelude::*;

use super::shell_metrics::workbench_title;
use crate::components::design_icon::Icon;

/// The bar, shown on a compact viewport.
#[component]
pub fn MobileTopBar(
    path: String,
    session_title: Option<String>,
    session_folder: Option<String>,
) -> Element {
    let title = workbench_title(
        &path,
        session_title.as_deref(),
        session_folder.as_deref(),
    );
    rsx! {
        header {
            class: "mobile-topbar",
            "data-testid": "mobile-topbar",
            button {
                type: "button",
                class: "mobile-topbar__menu",
                "data-testid": "mobile-topbar-menu",
                "aria-label": "Open sidebar",
                Icon { name: "menu", filled: false }
            }
            span { class: "mobile-topbar__title", {title} }
        }
    }
}
