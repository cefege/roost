//! A pane strip's "+": a new terminal or agent in the pane's folder.
//! Libero owns menu focus, keyboard navigation and dismissal.
//! `TerminalDeck` owns the two creation actions.

use dioxus::prelude::*;
use libero::components::{ActionIcon, Menu, MenuItem, Pictogram, use_menu};
use libero::sx::sx;

#[component]
pub fn PaneTabNewMenu(
    on_new_terminal: EventHandler<()>,
    on_new_agent: EventHandler<()>,
) -> Element {
    let menu = use_menu();
    let trigger_sx = use_hook(|| sx().flex_shrink("0"));
    let items = vec![
        MenuItem::new("New terminal")
            .leading(rsx! { Pictogram { icon: pictogram_icons_lucide::terminal::outlined, width: "1rem", height: "1rem" } })
            .onselect(move |_| on_new_terminal.call(()))
            .into(),
        MenuItem::new("New agent")
            .leading(rsx! { Pictogram { icon: pictogram_icons_lucide::bot::outlined, width: "1rem", height: "1rem" } })
            .onselect(move |_| on_new_agent.call(()))
            .into(),
    ];
    rsx! {
        Menu { state: menu, items,
            ActionIcon {
                icon: pictogram_icons_lucide::plus::outlined,
                aria_label: "New tab",
                attributes: menu.a11y_attributes(),
                sx: trigger_sx,
                "data-testid": "tab-new",
            }
        }
    }
}
