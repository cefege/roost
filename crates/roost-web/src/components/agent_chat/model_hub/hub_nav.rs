//! The hub's view list: the sidebar beside the content on a wide screen, and
//! the first screen of the phone's list → detail flow, where each row leads
//! into its view.

use dioxus::prelude::*;

use super::hub_logic::{HubSection, HubView};
use crate::components::md::{Chip, Icon, List, ListRow};

/// The element id of a view's row, so focus can return to it.
pub(super) fn nav_row_id(view: &HubView) -> String {
    format!("agent-model-hub-row-{}", view.id())
}

fn view_icon(section: &HubSection) -> &'static str {
    match section.view {
        _ if section.locked => "lock",
        HubView::Recent => "history",
        HubView::Roles => "tune",
        HubView::All => "apps",
        HubView::Provider(_) => "cloud_done",
    }
}

#[component]
pub(super) fn HubNav(
    sections: Vec<HubSection>,
    current: HubView,
    compact: bool,
    on_select: EventHandler<HubView>,
) -> Element {
    rsx! {
        nav {
            class: "model-hub__nav",
            "aria-label": "Model views",
            List {
                for section in sections {
                    {
                        let target = section.view.clone();
                        let is_current = section.view == current;
                        let count = section.count;
                        rsx! {
                            ListRow {
                                key: "{section.view.id()}",
                                id: nav_row_id(&section.view),
                                leading_icon: view_icon(&section).to_owned(),
                                headline: rsx! { "{section.label}" },
                                support: section.locked.then(|| rsx! { "Not signed in" }),
                                trailing: rsx! {
                                    if let Some(count) = count {
                                        Chip { label: count.to_string(), small: true }
                                    }
                                    if compact {
                                        Icon { name: "chevron_right".to_owned() }
                                    }
                                },
                                dense: true,
                                selected: is_current && !compact,
                                aria_current: (is_current && !compact).then(|| "true".to_owned()),
                                test_id: format!("agent-model-hub-view-{}", section.view.id()),
                                onclick: move |_| on_select.call(target.clone()),
                            }
                        }
                    }
                }
            }
        }
    }
}
