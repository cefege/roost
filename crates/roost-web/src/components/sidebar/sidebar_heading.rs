//! The sidebar heading: the "Sessions" label, the agent launcher, and the
//! Recent / Folders segmented control. The stock `Tabs` carry no panels, so
//! `SidebarRoot` decides which view is mounted.

use dioxus::prelude::*;

use super::sidebar_new_terminal::SidebarNewAgent;
use crate::components::ui::{Tabs, TabsList, TabsTrigger};

/// Which list the sidebar body shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SidebarView {
    #[default]
    Recent,
    Folders,
}

impl SidebarView {
    /// The `Tabs` value spelling.
    pub const fn as_value(self) -> &'static str {
        match self {
            Self::Recent => "recent",
            Self::Folders => "folders",
        }
    }

    /// Reads a `Tabs` value back; an unknown value falls back to Recent.
    pub fn from_value(value: &str) -> Self {
        match value {
            "folders" => Self::Folders,
            _ => Self::Recent,
        }
    }
}

/// The sidebar heading and its view switcher.
#[component]
pub fn SidebarHeading(view: SidebarView, on_view_change: EventHandler<SidebarView>) -> Element {
    rsx! {
        div { class: "flex flex-col gap-2 px-3 pt-3", "data-testid": "sidebar-heading",
            div { class: "flex items-center justify-between gap-2",
                span { class: "text-sm font-semibold", "Sessions" }
                SidebarNewAgent {}
            }
            Tabs {
                value: view.as_value().to_owned(),
                on_value_change: move |next: String| on_view_change.call(SidebarView::from_value(&next)),
                TabsList { class: "grid w-full grid-cols-2", aria_label: "Sidebar view",
                    TabsTrigger { value: "recent", has_panel: false, "Recent" }
                    TabsTrigger { value: "folders", has_panel: false, "Folders" }
                }
            }
        }
    }
}
