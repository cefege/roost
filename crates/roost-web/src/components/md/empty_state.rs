//! `EmptyState`: the icon, title, supporting line and optional action a surface
//! shows when it has nothing to list. Ported from
//! `apps/web/src/components/Settings/md/EmptyState.tsx`; search, file, browse and
//! settings surfaces compose it. Attaches `EmptyState.css` itself, because the
//! layout travels with the component rather than with any one route.

use dioxus::prelude::*;

use super::icon::{Icon, IconSize};
use super::stylesheet::{EMPTY_STATE_STYLESHEET_HREF, use_md_stylesheet};

/// An empty view.
#[component]
pub fn EmptyState(
    icon: String,
    title: String,
    supporting: Option<String>,
    action: Option<Element>,
) -> Element {
    use_md_stylesheet(EMPTY_STATE_STYLESHEET_HREF);
    rsx! {
        div { class: "md-empty-state",
            Icon { name: icon, size: IconSize::Lg, class: "md-empty-state__icon" }
            div { class: "md-empty-state__title", {title} }
            if let Some(supporting) = supporting {
                div { class: "md-empty-state__supporting", {supporting} }
            }
            if let Some(action) = action {
                {action}
            }
        }
    }
}
