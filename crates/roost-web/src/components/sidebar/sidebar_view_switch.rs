//! The sidebar's view selector: Folders keeps the account's manual order,
//! Sessions follows the latest terminal activity. The folder browser entry
//! sits beside it. SidebarRoot mounts it on desktop and in the compact drawer.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::store::sidebar::SidebarIntent;
use roost_client_core::store::ui::SidebarView;

use crate::components::ui::{IconButton, IconType, Tabs, TabsList, TabsTrigger};
use crate::pump::use_store;
use crate::router_state::use_navigate;
use crate::routes::Route;

/// The view selector.
#[component]
pub fn SidebarViewSwitch() -> Element {
    let pump = use_store();
    let navigate = use_navigate();
    let view = pump.core().borrow().store().ui.sidebar_view;
    let change_pump = pump.clone();
    rsx! {
        div { class: "flex items-center gap-2 px-3 pt-3", "data-testid": "sidebar-view-switch",
            Tabs {
                class: "min-w-0 flex-1",
                value: view.as_str().to_owned(),
                on_value_change: move |next: String| {
                    if let Some(view) = SidebarView::parse(&next) {
                        change_pump.dispatch(ClientEvent::Sidebar(SidebarIntent::SetView(view)));
                    }
                },
                TabsList { class: "w-full", aria_label: "Sidebar view",
                    TabsTrigger { value: "folders", test_id: "sidebar-view-folders", "Folders" }
                    TabsTrigger { value: "sessions", test_id: "sidebar-view-sessions", "Sessions" }
                }
            }
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
