//! The desktop sidebar region: the collapsed/expanded rail's container, the
//! `sidebar-desktop` slot the sidebar surface mounts into, and the resizer.
//! Ported from the `workbench-sidebar-region` arm of
//! `apps/web/src/components/layout/AppShell.tsx`; mounted by `AppShell` on a
//! desktop layout.
//!
//! THE REGION IS A SLOT: it owns the width, the collapse state and the resizer,
//! not a session list. A collapsed region is `inert` and `aria-hidden`, not
//! merely narrow — otherwise a keyboard reader tabs into a rail nobody can see.

use super::sidebar_resizer::SidebarResizer;
use dioxus::prelude::*;
use libero::{components::Box, sx::sx};

/// The DOM id of the region, which `AppShell` reads to decide whether focus
/// must move to the rail before a collapse.
pub const SIDEBAR_REGION_ID: &str = "workbench-sidebar-region";

/// The sidebar region, with the sidebar surface inside it.
#[component]
pub fn SidebarRegion(collapsed: bool, children: Element) -> Element {
    let flag = if collapsed { "true" } else { "false" };
    let region_sx = use_hook(|| {
        sx().position("relative")
            .min_height("0")
            .min_width("0")
            .overflow("hidden")
            .border_right("1px solid var(--sidenav-border)")
            .background("var(--workbench-sidebar)")
            .with("grid-area", "sidebar")
            .selector("&[data-collapsed=true]", sx().border_right("0"))
    });
    let sidebar_sx = use_hook(|| {
        sx().display("grid")
            .box_sizing("border-box")
            .height("100%")
            .min_height("0")
            .min_width("0")
            .width("var(--workbench-sidebar-expanded-width)")
            .with("grid-template-columns", "minmax(0, 1fr)")
            .with("grid-template-rows", "minmax(0, 1fr)")
            .overflow("hidden")
            .border_right("1px solid var(--workbench-sidebar-border)")
            .background("var(--workbench-sidebar)")
            .font_size("xs")
            .color("muted")
    });
    rsx! {
        Box {
            sx: region_sx,
            id: SIDEBAR_REGION_ID,
            class: "workbench-sidebar-region",
            "data-collapsed": flag,
            "aria-hidden": collapsed.then_some("true"),
            inert: collapsed.then_some(true),
            Box {
                component: "aside",
                sx: sidebar_sx,
                "data-testid": "sidebar-desktop",
                "data-collapsed": flag,
                {children}
            }
            if !collapsed {
                SidebarResizer {}
            }
        }
    }
}
