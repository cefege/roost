//! `SettingsNavigationSpecimen`: the desktop Settings rail reference on
//! `/design`, in the Settings shell's classes, navigation order and `ListRow`
//! link anatomy with Machines selected. `gallery.rs` mounts it. The rail data is
//! `components::settings_navigation`, the same definition the live Settings
//! shell reads, and the `settings-*` hook classes are the shell's own.

use dioxus::prelude::*;

use crate::components::settings_navigation::SETTINGS_GROUPS;
use crate::components::ui::{Icon, ListRow};
use crate::routes::Route;

/// The pane the specimen draws selected.
const SPECIMEN_SELECTED_PANE: &str = "machines";

/// The specimen. Rail links navigate in-app through `on_navigate`.
#[component]
pub fn SettingsNavigationSpecimen(on_navigate: EventHandler<String>) -> Element {
    rsx! {
        div { class: "flex h-[36rem] w-full min-w-0 bg-[var(--workbench-editor)] text-[var(--text-hi)]",
            aside { class: "flex w-56 shrink-0 flex-col gap-4 overflow-y-auto border-r border-[var(--workbench-border)] p-3", "aria-label": "Settings sections",
                h1 { class: "px-2 text-xl", "Settings" }
                for group in SETTINGS_GROUPS {
                    section { class: "flex flex-col gap-1",
                        h2 { class: "px-2 text-xs font-medium text-muted-foreground", {group.label} }
                        for pane in group.panes {
                            ListRow {
                                class: "w-full",
                                href: Route::Settings { pane: Some(pane.id.to_string()) }.to_path(),
                                on_navigate,
                                selected: pane.id == SPECIMEN_SELECTED_PANE,
                                aria_current: (pane.id == SPECIMEN_SELECTED_PANE).then(|| "page".to_string()),
                                leading: rsx! { Icon { icon: pane.icon } },
                                headline: rsx! { {pane.label} },
                            }
                        }
                    }
                }
            }
            main { class: "flex min-h-0 min-w-0 flex-1 flex-col bg-[var(--workbench-editor)]",
                header { class: "flex h-12 shrink-0 items-center border-b border-[var(--workbench-border)] bg-[var(--workbench-editor)] px-4",
                    h2 { class: "text-base text-[var(--text-hi)]", "Machines" }
                }
                div { class: "min-h-0 flex-1 overflow-y-auto bg-[var(--workbench-editor)]" }
            }
        }
    }
}
