//! The desktop title region: the brand, and the current route's context beside
//! it. Ported from `apps/web/src/components/layout/WorkbenchTitleBar.tsx`.
//!
//! The context text is `shell_metrics::workbench_title`, so the desktop bar and
//! the compact bar cannot disagree about what this path is called. The brand
//! link is the one control that always means "go home", which is why the context
//! is cut rather than allowed to push it out.

use dioxus::prelude::*;

use super::shell_metrics::workbench_title;
use crate::components::brand_mark::{BrandMark, TITLE_BAR_MARK_SIZE};
use crate::routes::Route;

/// The product name, and what a path that resolves to no more specific context
/// reads as. Shared with the compact bar, which shows it as its title.
pub const PRODUCT: &str = "Roost";

/// The title region, shown on every desktop path.
#[component]
pub fn TitleBar(
    path: String,
    session_title: Option<String>,
    session_folder: Option<String>,
) -> Element {
    let context = workbench_title(
        &path,
        session_title.as_deref(),
        session_folder.as_deref(),
    );
    rsx! {
        header { class: "workbench-titlebar",
            div { class: "workbench-titlebar__left",
                a {
                    class: "workbench-titlebar__brand",
                    href: Route::Home.to_path(),
                    "aria-label": "Roost home",
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
