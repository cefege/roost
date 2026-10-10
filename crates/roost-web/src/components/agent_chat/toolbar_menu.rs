//! One chat-toolbar menu: a compact trigger (a labelled picker or a bare icon)
//! and an anchored list of choices built on the deck's floating-menu kit, so
//! it dismisses, focuses and takes arrow keys like every other deck menu.
//! Used by `agent_chat::header` for the model, thinking and overflow menus.

use dioxus::prelude::*;

use crate::components::context_menu::{
    ANCHOR_DROP_PX, AnchoredMenuPos, CtxMenuItem, MenuFocusEdge, anchored_menu_pos,
    anchored_menu_surface_style, use_floating_menu_dismiss,
};
use crate::components::deck::deck_dom::{self, DeckContainer};
use crate::components::md::{Button, ButtonSize, ButtonVariant, Icon, IconButton, IconButtonSize};

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
    Icon { icon: &'static str },
}

#[component]
pub fn ToolbarMenu(
    menu_key: String,
    trigger: ToolbarTrigger,
    aria_label: String,
    items: Vec<ToolbarMenuItem>,
    on_choose: EventHandler<String>,
    /// Open above the trigger, for a trigger at the bottom of the pane.
    #[props(default)]
    opens_up: bool,
) -> Element {
    let mut open = use_signal(|| None::<AnchoredMenuPos>);
    // Distance from the containing block's bottom edge to the trigger's top,
    // when the menu rises instead of drops.
    let mut rise_px = use_signal(|| None::<f64>);
    let mut anchor = use_signal(|| None::<std::rc::Rc<MountedData>>);
    let container = try_use_context::<DeckContainer>();
    let trigger_id = format!("agent-chat-{menu_key}-trigger");
    let menu_id = format!("agent-chat-{menu_key}-menu");
    let open_menu = {
        let menu_id = menu_id.clone();
        move |edge: MenuFocusEdge| {
            let Some(box_) = anchor.peek().as_deref().and_then(deck_dom::client_box) else {
                return;
            };
            let width = deck_dom::viewport_width();
            let position = anchored_menu_pos(box_.left + box_.width, box_.bottom(), width);
            let origin = container
                .map(|container| container.origin())
                .unwrap_or_default();
            rise_px.set(opens_up.then(|| {
                let floor = if origin.height > 0.0 {
                    origin.top + origin.height
                } else {
                    viewport_height()
                };
                (floor - box_.top + ANCHOR_DROP_PX).max(0.0)
            }));
            open.set(Some(deck_dom::menu_pos_in(position, origin, width)));
            deck_dom::focus_menu(&menu_id, edge);
        }
    };
    let close_menu = {
        let trigger_id = trigger_id.clone();
        move |restore_focus: bool| {
            open.set(None);
            if restore_focus {
                deck_dom::focus_by_id(&trigger_id);
            }
        }
    };
    let mut toggle = {
        let mut open_menu = open_menu.clone();
        let mut close_menu = close_menu.clone();
        move || {
            if open.peek().is_some() {
                close_menu(false);
            } else {
                open_menu(MenuFocusEdge::First);
            }
        }
    };
    let mut open_by_key = open_menu;
    let on_trigger_key = move |event: KeyboardEvent| {
        let key = event.key().to_string();
        if key != "ArrowDown" && key != "ArrowUp" {
            return;
        }
        event.prevent_default();
        event.stop_propagation();
        open_by_key(if key == "ArrowDown" {
            MenuFocusEdge::First
        } else {
            MenuFocusEdge::Last
        });
    };
    let expanded = if open.read().is_some() {
        "true"
    } else {
        "false"
    };
    let trigger_element = match trigger {
        ToolbarTrigger::Picker { label } => rsx! {
            Button {
                variant: ButtonVariant::Ghost,
                size: ButtonSize::Sm,
                class: "agent-chat__picker",
                id: trigger_id.clone(),
                "aria-label": aria_label.clone(),
                "aria-haspopup": "menu",
                "aria-controls": menu_id.clone(),
                "aria-expanded": expanded,
                title: aria_label.clone(),
                onmounted: move |event: MountedEvent| anchor.set(Some(event.data())),
                onclick: move |_| toggle(),
                onkeydown: on_trigger_key,
                span { class: "agent-chat__picker-label", "{label}" }
                Icon { name: "expand_more" }
            }
        },
        ToolbarTrigger::Icon { icon } => rsx! {
            IconButton {
                icon,
                label: aria_label.clone(),
                size: IconButtonSize::IconSm,
                id: trigger_id.clone(),
                menu_popup: "menu",
                controls_id: menu_id.clone(),
                expanded: open.read().is_some(),
                title: aria_label.clone(),
                onmounted: move |event: MountedEvent| anchor.set(Some(event.data())),
                onclick: move |_| toggle(),
                onkeydown: on_trigger_key,
            }
        },
    };
    rsx! {
        {trigger_element}
        if let Some(position) = *open.read() {
            ToolbarMenuSurface {
                position,
                rise_px: *rise_px.read(),
                trigger_id,
                menu_id,
                items,
                on_choose: {
                    let mut close_menu = close_menu.clone();
                    move |value: String| {
                        close_menu(true);
                        on_choose.call(value);
                    }
                },
                on_close: close_menu,
            }
        }
    }
}

#[component]
fn ToolbarMenuSurface(
    position: AnchoredMenuPos,
    rise_px: Option<f64>,
    trigger_id: String,
    menu_id: String,
    items: Vec<ToolbarMenuItem>,
    on_choose: EventHandler<String>,
    on_close: EventHandler<bool>,
) -> Element {
    // A long model list scrolls inside the menu rather than off the pane.
    let placement = match rise_px {
        Some(rise) => format!(
            "top: auto; bottom: {rise}px; max-height: calc(var(--md-space-9) * 8); overflow-y: auto;"
        ),
        None => "max-height: calc(var(--md-space-9) * 8); overflow-y: auto;".to_owned(),
    };
    use_floating_menu_dismiss(
        EventHandler::new(move |()| on_close.call(false)),
        None,
        vec![trigger_id.clone(), menu_id.clone()],
    );
    let keys_menu_id = menu_id.clone();
    rsx! {
        div {
            id: menu_id,
            role: "menu",
            "aria-labelledby": trigger_id,
            class: "df-menu-enter agent-chat__menu",
            style: anchored_menu_surface_style(position, "calc(var(--md-space-7) * 7)", None, &placement),
            onkeydown: move |event: KeyboardEvent| {
                deck_dom::run_menu_keys(&event, &keys_menu_id, move || on_close.call(true), move || on_close.call(false));
            },
            for item in items {
                CtxMenuItem {
                    key: "{item.value}",
                    testid: format!("agent-chat-menu-{}", item.value),
                    danger: item.danger,
                    onclick: {
                        let value = item.value.clone();
                        move |_| on_choose.call(value.clone())
                    },
                    span { class: "agent-chat__menu-row", "data-selected": item.selected.then_some("true"),
                        span { class: "agent-chat__menu-check",
                            if item.selected { Icon { name: "check" } }
                        }
                        span { class: "agent-chat__menu-label", "{item.label}" }
                        if let Some(detail) = item.detail {
                            span { class: "agent-chat__menu-detail", "{detail}" }
                        }
                    }
                }
            }
        }
    }
}

/// The viewport's height, for a rising menu outside any deck container.
fn viewport_height() -> f64 {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()
            .and_then(|window| window.inner_height().ok())
            .and_then(|height| height.as_f64())
            .unwrap_or_default()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        0.0
    }
}
