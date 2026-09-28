//! The desktop title region: the brand, and the current route's context beside
//! it. Ported from `apps/web/src/components/layout/WorkbenchTitleBar.tsx`;
//! mounted by `AppShell` on a desktop layout.
//!
//! The context text is `shell_metrics::workbench_title`, so the desktop bar and
//! the compact bar cannot disagree about what this path is called.

use dioxus::prelude::*;

use super::shell_metrics::workbench_title;
use crate::components::brand_mark::{BrandMark, TITLE_BAR_MARK_SIZE};
use crate::router_state::use_navigate;
use crate::routes::Route;

/// The product name, and what a path with no more specific context reads as.
pub const PRODUCT: &str = "Roost";

/// The title region.
#[component]
pub fn TitleBar(
    path: String,
    session_title: Option<String>,
    session_folder: Option<String>,
) -> Element {
    let navigate = use_navigate();
    let home_path = Route::Home.to_path();
    let context = workbench_title(&path, session_title.as_deref(), session_folder.as_deref());
    rsx! {
        header { class: "workbench-titlebar",
            div { class: "workbench-titlebar__left",
                a {
                    class: "workbench-titlebar__brand",
                    href: home_path.clone(),
                    "aria-label": "Roost home",
                    onclick: move |event: MouseEvent| {
                        if event.modifiers().is_empty() {
                            event.prevent_default();
                            navigate.call(home_path.clone());
                        }
                    },
                    BrandMark { size: TITLE_BAR_MARK_SIZE }
                    span { class: "workbench-titlebar__product", {PRODUCT} }
                    if context != PRODUCT {
                        span { class: "workbench-titlebar__context", {context} }
                    }
                }
            }
        }
    }
}
