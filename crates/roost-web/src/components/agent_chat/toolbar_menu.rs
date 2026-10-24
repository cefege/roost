//! One chat-toolbar menu: a compact trigger (a labelled picker or a bare icon)
//! and a choice list on the ui `DropdownMenu`, which anchors beside the trigger
//! and owns its keys and dismissal. Used by `agent_chat::header` for the model,
//! thinking and overflow menus.

use dioxus::prelude::*;

use crate::components::ui::dropdown_menu::{
    DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger,
};
use crate::components::ui::menu_overlay::MenuSide;
use crate::components::ui::{Icon, IconType};

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
    Icon { icon: IconType },
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
    let mut open = use_signal(|| false);
    let side = if opens_up {
        MenuSide::Above
    } else {
        MenuSide::Below
    };
    let trigger_element = match trigger {
        ToolbarTrigger::Picker { label } => rsx! {
            DropdownMenuTrigger { ghost: true, class: "min-w-0 max-w-full",
                span { class: "min-w-0 truncate", "{label}" }
                Icon { icon: IconType::ChevronDown, class: "size-4" }
            }
        },
        ToolbarTrigger::Icon { icon } => rsx! {
            DropdownMenuTrigger { ghost: true, class: "h-7 w-7 p-0",
                Icon { icon, class: "size-4" }
                // The icon has no text of its own; this keeps the trigger named.
                span { class: "sr-only", "{aria_label}" }
            }
        },
    };
    rsx! {
        DropdownMenu {
            open: open(),
            on_open_change: move |next: bool| open.set(next),
            test_id: format!("agent-chat-{menu_key}-menu"),
            side,
            {trigger_element}
            DropdownMenuContent { class: "w-auto min-w-48 max-h-[60vh] flex select-none flex-col overflow-y-auto",
                for item in items {
                    DropdownMenuItem {
                        key: "{item.value}",
                        test_id: format!("agent-chat-menu-{}", item.value),
                        class: if item.danger { "text-destructive" } else { "" },
                        onclick: {
                            let value = item.value.clone();
                            move |_: MouseEvent| on_choose.call(value.clone())
                        },
                        span {
                            class: "flex w-full min-w-0 items-center gap-2",
                            "data-selected": item.selected.then_some("true"),
                            span { class: "flex size-4 shrink-0 items-center justify-center",
                                if item.selected {
                                    Icon { icon: IconType::Check, class: "size-4" }
                                }
                            }
                            span { class: "min-w-0 flex-1 truncate text-left", "{item.label}" }
                            if let Some(detail) = item.detail {
                                span { class: "shrink-0 text-muted-foreground", "{detail}" }
                            }
                        }
                    }
                }
            }
        }
    }
}
