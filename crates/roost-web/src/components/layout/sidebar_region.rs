//! The desktop sidebar region: the collapsed/expanded rail's container, the
//! `sidebar-desktop` slot the sidebar surface mounts into, and the resizer.
//! Ported from the `workbench-sidebar-region` arm of
//! `apps/web/src/components/layout/AppShell.tsx`; mounted by `AppShell` on a
//! desktop layout.
//!
//! THE REGION IS A SLOT: it owns the width, the collapse state and the resizer,
//! not a session list. A collapsed region is `inert` and `aria-hidden`, not
//! merely narrow — otherwise a keyboard reader tabs into a rail nobody can see.

use dioxus::prelude::*;

use super::sidebar_resizer::SidebarResizer;

/// The DOM id of the region, which `AppShell` reads to decide whether focus
/// must move to the rail before a collapse.
pub const SIDEBAR_REGION_ID: &str = "workbench-sidebar-region";

/// The sidebar region, with the sidebar surface inside it.
#[component]
pub fn SidebarRegion(collapsed: bool, children: Element) -> Element {
    let flag = if collapsed { "true" } else { "false" };
    rsx! {
        div {
            id: SIDEBAR_REGION_ID,
            class: "workbench-sidebar-region",
            "data-collapsed": flag,
            "aria-hidden": collapsed.then_some("true"),
            inert: collapsed.then_some(true),
            aside {
                class: "workbench-sidebar",
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
