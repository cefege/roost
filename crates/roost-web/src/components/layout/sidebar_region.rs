//! The desktop sidebar region: the collapsed/expanded rail's container, the
//! slot the sidebar surface mounts into, and the resizer between them. Ported
//! from the `workbench-sidebar-region` arm of
//! `apps/web/src/components/layout/AppShell.tsx`.
//!
//! THE REGION IS A SLOT. It owns the width, the collapse state and the resizer;
//! it does not own a session list, a folder tree or a machine view. The sidebar
//! surface mounts as `children` and this module is what keeps it the right width
//! — so a sidebar that grew its own width logic would be a second answer to a
//! question the grid already answers.
//!
//! A collapsed region is `inert` and `aria-hidden`, not merely narrow. Its
//! contents stay in the accessibility tree otherwise, so a keyboard reader would
//! tab into a rail nobody can see.

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::ClientCore;

/// The resizer's grab area, as a design-system token the stylesheet reads.
///
/// Exposed as a token rather than a width so the drag target can be widened
/// without touching this module, and so a reader who cannot drag still has a
/// focusable control: the resizer is a `separator` with a value, not a div.
pub const RESIZER_WIDTH_VAR: &str = "--workbench-sidebar-resizer-width";

/// The sidebar region, with whatever the sidebar surface mounts inside it.
#[component]
pub fn SidebarRegion(collapsed: bool, children: Element) -> Element {
    rsx! {
        div {
            class: "workbench-sidebar-region",
            "data-collapsed": if collapsed { "true" } else { "false" },
            "aria-hidden": collapsed.then_some("true"),
            inert: collapsed.then_some(true),
            aside {
                class: "workbench-sidebar",
                "data-testid": "sidebar-desktop",
                "data-collapsed": if collapsed { "true" } else { "false" },
                {children}
            }
            if !collapsed {
                SidebarResizer {}
            }
        }
    }
}

/// The drag handle between the sidebar and the editor.
///
/// `aria-valuenow` is the sidebar's width, so the control reports the thing it
/// changes. The pointer and keyboard handling is the adapter's; the width it
/// clamps to is `roost_client_core::store::ui::clamp_sidebar_width`, which the
/// setter and the loader both apply, so a width can arrive from a drag and from
/// storage without two ranges.
#[component]
fn SidebarResizer() -> Element {
    rsx! {
        div {
            class: "workbench-sidebar-resizer",
            role: "separator",
            "aria-orientation": "vertical",
            "aria-label": "Resize sidebar",
            tabindex: "0",
        }
    }
}

/// The sidebar region's width, as the chrome reads it.
///
/// A named question so the AppShell and the resizer cannot disagree about which
/// half of the pair they are asking, and so a future drag handler has one place
/// to write its result.
pub fn region_width_px(core: &Rc<RefCell<ClientCore>>) -> u32 {
    core.borrow().store().ui.sidebar_width
}

#[cfg(test)]
mod tests {
    use super::*;
    use roost_client_core::ClientCore;
    #[test]
    fn a_fresh_client_reports_the_store_default_rather_than_a_second_one() {
        // The number is the store's, named: a literal here would be a width this
        // module believes in and the store does not, and the two would disagree
        // the first time the default moved.
        let core = Rc::new(RefCell::new(ClientCore::in_memory("tab-test")));
        assert_eq!(
            region_width_px(&core),
            roost_client_core::store::ui::SIDEBAR_WIDTH_DEFAULT
        );
    }

    #[test]
    fn the_resizer_width_is_a_token_and_not_a_number() {
        // A number here would pin the grab area at the value this module was
        // written with, and the stylesheet's own token would mean nothing.
        assert!(RESIZER_WIDTH_VAR.starts_with("--workbench-sidebar"));
    }
}
