//! A pane strip's "+": a new terminal in the pane's folder, or, when the
//! coordinator runs the built-in agent, a menu offering a terminal or an agent.
//! Anchored, dismissed and keyboard-driven the way `arrange_menu` is; the
//! creation itself is `TerminalDeck`'s, through the two callbacks.

use dioxus::prelude::*;

use super::deck_dom::{self, DeckContainer};
use crate::components::context_menu::{
    AnchoredMenuPos, CtxMenuItem, MenuFocusEdge, anchored_menu_pos, anchored_menu_surface_style,
    use_floating_menu_dismiss,
};
use crate::components::md::{Icon, IconButton, IconButtonSize};

#[component]
pub fn PaneTabNewMenu(
    pane_id: String,
    agent_enabled: bool,
    on_new_terminal: EventHandler<()>,
    on_new_agent: EventHandler<()>,
) -> Element {
    let mut open = use_signal(|| None::<AnchoredMenuPos>);
    let mut trigger = use_signal(|| None::<std::rc::Rc<MountedData>>);
    let container = try_use_context::<DeckContainer>();
    let trigger_id = format!("tab-new-{pane_id}");
    let menu_id = format!("tab-new-menu-{pane_id}");
    if !agent_enabled {
        return rsx! {
            IconButton {
                icon: "add",
                label: "New terminal — same folder and server",
                size: IconButtonSize::IconSm,
                class: "df-tab-new workbench-pane-tab-control",
                "data-testid": "tab-new",
                title: "New terminal in this folder (or double-click the empty bar)",
                onclick: move |_| on_new_terminal.call(()),
            }
        };
    }
    let open_menu = {
        let menu_id = menu_id.clone();
        move |edge: MenuFocusEdge| {
            let Some(anchor) = trigger.peek().as_deref().and_then(deck_dom::client_box) else {
                return;
            };
            let width = deck_dom::viewport_width();
            let position = anchored_menu_pos(anchor.left + anchor.width, anchor.bottom(), width);
            let origin = container
                .map(|container| container.origin())
                .unwrap_or_default();
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
    let mut open_on_click = open_menu.clone();
    let mut open_on_key = open_menu;
    let mut close_on_click = close_menu.clone();
    rsx! {
        IconButton {
            icon: "add",
            label: "New tab",
            size: IconButtonSize::IconSm,
            class: "df-tab-new workbench-pane-tab-control",
            "data-testid": "tab-new",
            id: trigger_id.clone(),
            menu_popup: "menu",
            controls_id: menu_id.clone(),
            expanded: open.read().is_some(),
            title: "New terminal or agent in this folder",
            onmounted: move |event: MountedEvent| trigger.set(Some(event.data())),
            onclick: move |_| {
                if open.peek().is_some() {
                    close_on_click(false);
                } else {
                    open_on_click(MenuFocusEdge::First);
                }
            },
            onkeydown: move |event: KeyboardEvent| {
                let key = event.key().to_string();
                if key != "ArrowDown" && key != "ArrowUp" {
                    return;
                }
                event.prevent_default();
                event.stop_propagation();
                open_on_key(if key == "ArrowDown" { MenuFocusEdge::First } else { MenuFocusEdge::Last });
            },
        }
        if let Some(position) = *open.read() {
            NewTabMenuSurface {
                position,
                trigger_id,
                menu_id,
                on_new_terminal: {
                    let mut close_menu = close_menu.clone();
                    move |()| {
                        close_menu(true);
                        on_new_terminal.call(());
                    }
                },
                on_new_agent: {
                    let mut close_menu = close_menu.clone();
                    move |()| {
                        close_menu(true);
                        on_new_agent.call(());
                    }
                },
                on_close: close_menu,
            }
        }
    }
}

#[component]
fn NewTabMenuSurface(
    position: AnchoredMenuPos,
    trigger_id: String,
    menu_id: String,
    on_new_terminal: EventHandler<()>,
    on_new_agent: EventHandler<()>,
    on_close: EventHandler<bool>,
) -> Element {
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
            "data-testid": "tab-new-menu",
            class: "df-menu-enter",
            style: anchored_menu_surface_style(position, "calc(var(--md-space-7) * 5)", None, ""),
            onkeydown: move |event: KeyboardEvent| {
                deck_dom::run_menu_keys(&event, &keys_menu_id, move || on_close.call(true), move || on_close.call(false));
            },
            CtxMenuItem { testid: "tab-new-terminal", onclick: move |_| on_new_terminal.call(()),
                NewTabRow { icon: "terminal", label: "New terminal" }
            }
            CtxMenuItem { testid: "tab-new-agent", onclick: move |_| on_new_agent.call(()),
                NewTabRow { icon: "smart_toy", label: "New agent" }
            }
        }
    }
}

#[component]
fn NewTabRow(icon: &'static str, label: &'static str) -> Element {
    rsx! {
        span { class: "workbench-pane-tab-new-row",
            Icon { name: icon }
            span { "{label}" }
        }
    }
}
