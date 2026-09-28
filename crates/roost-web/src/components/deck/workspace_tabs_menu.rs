//! The phone terminal grid's overflow menu: close all / select tabs, or in
//! selection mode select all / close selected. `MobileDeckBar`'s sheet
//! supplies the actions; the shared context-menu primitives draw the items.
//! Ports `apps/web/src/components/deck/WorkspaceTabsMenu.tsx`.

use dioxus::prelude::*;

use super::deck_dom;
use crate::components::context_menu::{
    AnchoredMenuPos, CtxMenuItem, MenuFocusEdge, anchored_menu_pos, anchored_menu_surface_style,
    use_floating_menu_dismiss,
};
use crate::components::md::IconButton;

const TRIGGER_ID: &str = "workspace-tabs-menu-trigger";
const MENU_ID: &str = "workspace-tabs-menu-popup";

/// The ⋮ trigger and its menu.
#[component]
pub fn WorkspaceTabsMenu(
    selection_mode: bool,
    on_close_all: EventHandler<()>,
    on_select_tabs: EventHandler<()>,
    on_select_all: EventHandler<()>,
    on_close_selected: EventHandler<()>,
) -> Element {
    let mut open = use_signal(|| None::<AnchoredMenuPos>);
    let mut trigger = use_signal(|| None::<std::rc::Rc<MountedData>>);
    let mut open_menu = move |edge: MenuFocusEdge| {
        let Some(anchor) = trigger.peek().as_deref().and_then(deck_dom::client_box) else {
            return;
        };
        open.set(Some(anchored_menu_pos(
            anchor.left + anchor.width,
            anchor.bottom(),
            deck_dom::viewport_width(),
        )));
        deck_dom::focus_menu(MENU_ID, edge);
    };
    let mut close_menu = move |restore_focus: bool| {
        open.set(None);
        if restore_focus {
            deck_dom::focus_by_id(TRIGGER_ID);
        }
    };
    let mut choose = move |action: EventHandler<()>| {
        close_menu(true);
        action.call(());
    };
    rsx! {
        IconButton {
            id: TRIGGER_ID,
            icon: "more_vert",
            label: "More options",
            "data-testid": "workspace-tabs-menu",
            menu_popup: "menu",
            controls_id: MENU_ID,
            expanded: open.read().is_some(),
            onmounted: move |event: MountedEvent| trigger.set(Some(event.data())),
            onclick: move |_| {
                if open.peek().is_some() {
                    close_menu(false);
                } else {
                    open_menu(MenuFocusEdge::First);
                }
            },
            onkeydown: move |event: KeyboardEvent| {
                let key = event.key().to_string();
                if key != "ArrowDown" && key != "ArrowUp" {
                    return;
                }
                event.prevent_default();
                event.stop_propagation();
                open_menu(if key == "ArrowDown" { MenuFocusEdge::First } else { MenuFocusEdge::Last });
            },
        }
        if let Some(position) = *open.read() {
            WorkspaceTabsMenuSurface {
                position,
                on_close: move |restore| close_menu(restore),
                if !selection_mode {
                    CtxMenuItem { testid: "workspace-tabs-close-all", danger: true, onclick: move |_| choose(on_close_all), "Close all tabs" }
                    CtxMenuItem { testid: "workspace-tabs-select", onclick: move |_| choose(on_select_tabs), "Select tabs" }
                } else {
                    CtxMenuItem { testid: "workspace-tabs-select-all", onclick: move |_| choose(on_select_all), "Select all" }
                    CtxMenuItem { testid: "workspace-tabs-close-selected", danger: true, onclick: move |_| choose(on_close_selected), "Close selected tabs" }
                }
            }
        }
    }
}

#[component]
fn WorkspaceTabsMenuSurface(
    position: AnchoredMenuPos,
    on_close: EventHandler<bool>,
    children: Element,
) -> Element {
    use_floating_menu_dismiss(
        EventHandler::new(move |()| on_close.call(false)),
        None,
        vec![TRIGGER_ID.to_owned(), MENU_ID.to_owned()],
    );
    rsx! {
        div {
            role: "menu",
            "aria-label": "Workspace terminal actions",
            id: MENU_ID,
            "aria-labelledby": TRIGGER_ID,
            "data-testid": "workspace-tabs-menu-popup",
            class: "df-menu-enter",
            style: anchored_menu_surface_style(position, "calc(var(--md-space-9) * 6)", Some(70), ""),
            onkeydown: move |event: KeyboardEvent| {
                deck_dom::run_menu_keys(&event, MENU_ID, move || on_close.call(true), move || on_close.call(false));
            },
            {children}
        }
    }
}
