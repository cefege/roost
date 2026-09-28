//! `SettingsNavigationSpecimen`: the desktop Settings rail reference on
//! `/design`, in the Settings shell's classes, navigation order and `ListRow`
//! link anatomy with Machines selected. Ported from
//! `apps/web/src/components/design/SettingsNavigationSpecimen.tsx`; `gallery.rs`
//! mounts it. The rail data is `components::settings_navigation`, the same
//! definition the live Settings shell reads.

use dioxus::prelude::*;

use crate::components::md::{Icon, ListRow};
use crate::components::settings_navigation::SETTINGS_GROUPS;
use crate::routes::Route;

/// The pane the specimen draws selected.
const SPECIMEN_SELECTED_PANE: &str = "machines";

/// The specimen. Rail links navigate in-app through `on_navigate`.
#[component]
pub fn SettingsNavigationSpecimen(on_navigate: EventHandler<String>) -> Element {
    rsx! {
        div { class: "settings-shell", style: "height: calc(var(--md-space-9) * 6);",
            aside { class: "settings-rail", "aria-label": "Settings sections",
                h1 { class: "settings-rail__title", "Settings" }
                for group in SETTINGS_GROUPS {
                    section { class: "settings-rail__group",
                        h2 { class: "settings-rail__group-label", {group.label} }
                        for pane in group.panes {
                            ListRow {
                                class: "settings-rail__item",
                                href: Route::Settings { pane: Some(pane.id.to_string()) }.to_path(),
                                on_navigate,
                                selected: pane.id == SPECIMEN_SELECTED_PANE,
                                aria_current: (pane.id == SPECIMEN_SELECTED_PANE).then(|| "page".to_string()),
                                leading: rsx! { Icon { name: pane.icon } },
                                headline: rsx! { {pane.label} },
                            }
                        }
                    }
                }
            }
            main { class: "settings-main",
                header { class: "settings-topbar",
                    h2 { class: "settings-topbar__title", "Machines" }
                }
                div { class: "settings-content" }
            }
        }
    }
}
