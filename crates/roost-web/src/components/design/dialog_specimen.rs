//! A design dialog with a nested menu and observable selection state.
//! The modal owner supplies its close action; Libero owns each overlay's focus.
//! Nested Escape must leave this dialog and its selection state intact.

use dioxus::prelude::*;
use libero::components::{Button, Dialog, Flex, Menu, MenuEntry, MenuItem, Text, use_menu};

#[component]
pub(super) fn DialogSpecimen(on_close: EventHandler<()>) -> Element {
    let menu = use_menu();
    let mut selected = use_signal(|| "No action selected");
    let items: Vec<MenuEntry> = vec![
        MenuItem::new("Keep this session")
            .onselect(move |_| selected.set("Keep this session"))
            .into(),
        MenuItem::new("Show session details")
            .onselect(move |_| selected.set("Show session details"))
            .into(),
    ];
    rsx! {
        Dialog { title: "Design specimen dialog", size: "md",
            Flex { direction: "column", gap: "md",
                Text { size: "sm", "The nested menu keeps the dialog open when dismissed." }
                Menu { state: menu, items,
                    Button { variant: "outlined", size: "sm",
                        attributes: menu.a11y_attributes(), "Dialog actions"
                    }
                }
                Text { size: "sm", role: "status", "{selected}" }
                Button { variant: "outlined", size: "sm", onclick: move |_| on_close.call(()), "Close dialog" }
            }
        }
    }
}
