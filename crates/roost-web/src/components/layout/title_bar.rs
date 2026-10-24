//! The desktop title region: the brand, and the current route's context beside
//! it. Ported from `apps/web/src/components/layout/WorkbenchTitleBar.tsx`;
//! mounted by `AppShell` on a desktop layout.
//!
//! The context text is `shell_metrics::workbench_title`, so the desktop bar and
//! the compact bar cannot disagree about what this path is called.

use dioxus::prelude::*;
use libero::{
    components::{Box, Text},
    sx::sx,
};

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
    let titlebar_sx = use_hook(|| {
        sx().display("flex")
            .min_width("0")
            .align_items("center")
            .border_bottom("1px solid var(--lsx-paper-border-color)")
            .background("var(--workbench-titlebar)")
            .padding_inline("sm")
            .with("grid-area", "titlebar")
    });
    let brand_sx = use_hook(|| {
        sx().display("inline-flex")
            .min_width("0")
            .align_items("center")
            .gap("sm")
            .overflow("hidden")
            .color("ink")
            .text_decoration("none")
            .selector(
                "&:focus-visible",
                sx().outline("2px solid var(--lsx-focus-ring-color)")
                    .outline_offset("-2px"),
            )
    });
    let context_sx = use_hook(|| {
        sx().min_width("0")
            .overflow("hidden")
            .with("overscroll-behavior-y", "auto")
            .text_overflow("ellipsis")
            .white_space("nowrap")
            .font_size("xs")
            .color("muted")
            .media("(max-width: 1100px)", sx().display("none"))
    });
    let product_sx = use_hook(|| {
        sx().white_space("nowrap")
            .font_size("sm")
            .font_weight("500")
    });
    rsx! {
        Box { component: "header", sx: titlebar_sx,
            Box {
                component: "a",
                sx: brand_sx,
                href: home_path.clone(),
                "aria-label": "Roost home",
                onclick: move |event: MouseEvent| {
                    if event.modifiers().is_empty() {
                        event.prevent_default();
                        navigate.call(home_path.clone());
                    }
                },
                BrandMark { size: TITLE_BAR_MARK_SIZE }
                Text { component: "span", sx: product_sx, "{PRODUCT}" }
                if context != PRODUCT {
                    Text { component: "span", sx: context_sx, "{context}" }
                }
            }
        }
    }
}
