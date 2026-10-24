//! One chat-toolbar menu, with Libero owning its keyboard navigation and dismissal.
//! Used by `agent_chat::header` for the model, thinking and overflow menus.

use dioxus::prelude::*;
use libero::components::{
    ActionIcon, Box, Button, Menu, MenuEntry, MenuItem, Pictogram, SvgData, Text, use_menu,
};
use libero::sx::sx;

/// One choice in a toolbar menu.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolbarMenuItem {
    pub value: String,
    pub label: String,
    /// Muted text after the label, such as a model's provider.
    pub detail: Option<String>,
    pub selected: bool,
    pub danger: bool,
}

impl ToolbarMenuItem {
    pub fn choice(value: impl Into<String>, label: impl Into<String>, selected: bool) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            detail: None,
            selected,
            danger: false,
        }
    }
}

/// How the trigger draws.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolbarTrigger {
    /// Current value plus a chevron, as a picker.
    Picker { label: String },
    /// A bare icon, as an overflow button.
    Icon { icon: SvgData },
}

#[component]
pub fn ToolbarMenu(
    menu_key: String,
    trigger: ToolbarTrigger,
    aria_label: String,
    items: Vec<ToolbarMenuItem>,
    on_choose: EventHandler<String>,
    /// Prefer opening above the trigger, for a trigger at the bottom of the
    /// pane; the menu still drops when there is more room below.
    #[props(default)]
    opens_up: bool,
) -> Element {
    let menu = use_menu();
    let check_slot_sx = use_hook(|| {
        sx().display("flex")
            .width("1rem")
            .height("1rem")
            .flex_shrink("0")
            .align_items("center")
            .justify_content("center")
    });
    let trigger_text_sx = use_hook(|| {
        sx().min_width("0")
            .overflow("hidden")
            .with("overscroll-behavior-y", "auto")
            .text_overflow("ellipsis")
            .white_space("nowrap")
    });
    let trigger_sx = use_hook(|| sx().min_width("0").max_width("100%"));
    let items = items
        .into_iter()
        .map(|item| {
            let selected = item.selected;
            let label = item.label;
            let detail = item.detail;
            let value = item.value;
            MenuItem::new(label.clone())
                .leading(rsx! {
                    Box { sx: check_slot_sx.clone(),
                        if selected {
                            Pictogram { icon: pictogram_icons_lucide::check::outlined, width: "1rem", height: "1rem" }
                        }
                    }
                })
                .description(detail.unwrap_or_default())
                .disabled(false)
                .onselect(move |_| on_choose.call(value.clone()))
                .into()
        })
        .collect::<Vec<MenuEntry>>();
    let _ = opens_up;
    let _ = aria_label;
    rsx! {
        Menu { state: menu, items,
            match trigger {
                ToolbarTrigger::Picker { label } => rsx! {
                    Button {
                        variant: "standard",
                        sx: trigger_sx,
                        attributes: menu.a11y_attributes(),
                        "aria-label": aria_label,
                        "data-testid": format!("agent-chat-{menu_key}-menu"),
                        Text { sx: trigger_text_sx, "{label}" }
                        Pictogram { icon: pictogram_icons_lucide::chevron_down::outlined, width: "1rem", height: "1rem" }
                    }
                },
                ToolbarTrigger::Icon { icon } => rsx! {
                    ActionIcon {
                        icon,
                        aria_label,
                        attributes: menu.a11y_attributes(),
                        "data-testid": format!("agent-chat-{menu_key}-menu"),
                    }
                },
            }
        }
    }
}
