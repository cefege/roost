//! One chat-toolbar menu: a compact trigger (a labelled picker or a bare icon)
//! and an anchored list of choices built on the deck's floating-menu kit, so
//! it dismisses, focuses and takes arrow keys like every other deck menu.
//! Used by `agent_chat::header` for the model, thinking and overflow menus.

use dioxus::prelude::*;

use crate::components::context_menu::{
    AnchoredMenuPos, CtxMenuItem, MenuFocusEdge, anchored_menu_surface_style,
    use_floating_menu_dismiss,
};
use crate::components::deck::deck_dom::{self, ClientBox, DeckContainer};
use crate::components::md::{Button, ButtonSize, ButtonVariant, Icon, IconButton, IconButtonSize};
use crate::components::menu_placement::{MenuPlacement, place_menu};

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
    /// Prefer opening above the trigger, for a trigger at the bottom of the
    /// pane; the menu still drops when there is more room below.
    #[props(default)]
    opens_up: bool,
) -> Element {
    let mut open = use_signal(|| None::<MenuPlacement>);
    let mut anchor = use_signal(|| None::<std::rc::Rc<MountedData>>);
    let container = try_use_context::<DeckContainer>();
    let trigger_id = format!("agent-chat-{menu_key}-trigger");
    let menu_id = format!("agent-chat-{menu_key}-menu");
    let open_menu = {
        let menu_id = menu_id.clone();
        move |edge: MenuFocusEdge| {
            let Some(trigger) = anchor.peek().as_deref().and_then(deck_dom::client_box) else {
                return;
            };
            let viewport = ClientBox {
                left: 0.0,
                top: 0.0,
                width: deck_dom::viewport_width(),
                height: viewport_height(),
            };
            // Outside a deck the viewport is both the bounds and the
            // containing block of a fixed popup.
            let deck = container
                .map(|container| container.origin())
                .filter(|origin| origin.width > 0.0)
                .unwrap_or(viewport);
            open.set(Some(place_menu(trigger, deck, deck, opens_up)));
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
        if let Some(placement) = *open.read() {
            ToolbarMenuSurface {
                placement,
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
    placement: MenuPlacement,
    trigger_id: String,
    menu_id: String,
    items: Vec<ToolbarMenuItem>,
    on_choose: EventHandler<String>,
    on_close: EventHandler<bool>,
) -> Element {
    // The placement names every edge and the height cap, so the anchored
    // position is overridden and only the surface chrome is kept.
    let placement_style = placement.style();
    let chrome_origin = AnchoredMenuPos { right: 0.0, y: 0.0 };
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
            style: anchored_menu_surface_style(chrome_origin, "calc(var(--md-space-7) * 7)", None, &placement_style),
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
