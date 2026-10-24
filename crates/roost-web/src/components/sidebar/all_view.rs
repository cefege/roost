//! The Folders panel body: the no-machines empty state until a worker exists,
//! then the always-mounted `FolderList`, interactive only while the panel is.
//! Ports `apps/web/src/components/sidebar/AllView.tsx`; `SidebarRoot` renders it.

use dioxus::prelude::*;

use super::folder_list::FolderList;
use super::sidebar_empty_state::{EmptyStateKind, SidebarEmptyState};
use crate::pump::use_store;
use libero::components::Flex;
use libero::sx::sx;

/// The Folders panel.
#[component]
pub fn AllView(active: bool, query: String) -> Element {
    let pump = use_store();
    let no_machines = pump.core().borrow().store().workers.is_empty();
    let folder_list_active = active && !no_machines;
    let root_sx = use_hook(|| sx().flex("1").min_height("0").gap("0"));
    let panel_sx = use_hook(|| {
        sx().flex("1")
            .min_height("0")
            .gap("0")
            .selector("[data-active=false]", sx().display("none"))
    });
    rsx! {
        Flex { direction: "column", sx: root_sx, "data-testid": "all-view",
            if no_machines {
                SidebarEmptyState { kind: EmptyStateKind::NoMachines }
            } else {
                Flex {
                    direction: "column",
                    sx: panel_sx,
                    "data-active": if folder_list_active { "true" } else { "false" },
                    inert: (!folder_list_active).then_some(true),
                    "aria-hidden": (!folder_list_active).then_some("true"),
                    FolderList { active: folder_list_active, query }
                }
            }
        }
    }
}
