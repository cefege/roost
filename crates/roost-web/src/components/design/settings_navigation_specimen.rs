//! `SettingsNavigationSpecimen`: the desktop Settings rail reference on
//! `/design`, in the Settings shell's classes, navigation order and `ListRow`
//! link anatomy with Machines selected. `gallery.rs` mounts it. The rail data is
//! `components::settings_navigation`, the same definition the live Settings
//! shell reads, and the `settings-*` hook classes are the shell's own.

use dioxus::prelude::*;

use crate::components::settings_navigation::SETTINGS_GROUPS;
use crate::components::ui::{ListRow, SidenavGroupLabel};
use crate::routes::Route;

/// The pane the specimen draws selected.
const SPECIMEN_SELECTED_PANE: &str = "machines";

/// The specimen. Rail links navigate in-app through `on_navigate`.
#[component]
pub fn SettingsNavigationSpecimen(on_navigate: EventHandler<String>) -> Element {
    rsx! {
        div { class: "flex h-[36rem] w-full min-w-0 bg-background text-foreground",
            aside { class: "flex w-72 shrink-0 flex-col gap-4 overflow-y-auto border-r border-sidenav-border bg-sidenav p-4", "aria-label": "Settings sections",
                h1 { class: "px-2 text-lg font-semibold text-foreground", "Settings" }
                for group in SETTINGS_GROUPS {
                    section { class: "flex flex-col gap-1",
                        SidenavGroupLabel { "{group.label}" }
                        for pane in group.panes {
                            ListRow {
                                href: Route::Settings { pane: Some(pane.id.to_string()) }.to_path(),
                                on_navigate,
                                selected: pane.id == SPECIMEN_SELECTED_PANE,
                                aria_current: (pane.id == SPECIMEN_SELECTED_PANE).then(|| "page".to_string()),
                                leading_icon: pane.icon,
                                headline: rsx! { span { title: pane.description, {pane.label} } },
                            }
                        }
                    }
                }
            }
            main { class: "flex min-h-0 min-w-0 flex-1 flex-col bg-background",
                header { class: "flex h-14 shrink-0 items-center border-b border-border bg-background px-4",
                    h2 { class: "text-base font-semibold text-foreground", "Machines" }
                }
                div { class: "min-h-0 flex-1 overflow-y-auto bg-background" }
            }
        }
    }
}
