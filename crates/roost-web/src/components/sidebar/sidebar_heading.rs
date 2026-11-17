//! Folder heading and entry to the shared filesystem browser.
//! Mounted by SidebarRoot on desktop and in the compact drawer.
//! Navigation does not create terminals or choose a machine independently.

use crate::components::ui::{IconButton, IconType};
use crate::router_state::use_navigate;
use crate::routes::Route;
use dioxus::prelude::*;

/// The sidebar heading.
#[component]
pub fn SidebarHeading() -> Element {
    let navigate = use_navigate();
    rsx! {
        div { class: "flex items-center justify-between gap-2 px-3 pt-3", "data-testid": "sidebar-heading",
            span { class: "text-sm font-semibold", "Folders" }
            IconButton {
                icon: IconType::Plus,
                label: "Browse folders",
                "data-testid": "sidebar-browse-folders",
                onclick: move |_| {
                    tracing::info!(target: "sidebar", "folder browser opened");
                    navigate.call(Route::Browse { worker_fp: None }.to_path());
                },
            }
        }
    }
}
