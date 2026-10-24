//! Port of rust-ui `context_menu`: a controlled root whose trigger region opens
//! a fixed `role="menu"` panel at the pointer on right-click. Shared state and
//! items live in `ui/menu_overlay.rs` and `ui/menu_items.rs`; focus and key
//! helpers come from `components::context_menu`.

use dioxus::prelude::*;
use tw_merge::tw_merge;

use super::menu_items::{MenuItemHooks, render_menu_group, render_menu_item, render_menu_label};
use super::menu_overlay::{
    MenuContext, MenuPlacement, MenuRoot, MenuSide, MenuSurface, provide_menu_root, request_open,
};
use super::separator::Separator;
use crate::components::context_menu::MenuFocusEdge;
use crate::components::ui::field::scoped_element_id;

#[component]
pub fn ContextMenu(
    open: bool,
    on_open_change: EventHandler<bool>,
    #[props(into, optional)] test_id: Option<String>,
    #[props(into, optional)] aria_label: Option<String>,
    children: Element,
) -> Element {
    let trigger_id = use_hook(|| scoped_element_id("context-menu-trigger"));
    let menu_id = use_hook(|| scoped_element_id("context-menu"));
    provide_menu_root(MenuRoot {
        open,
        on_open_change,
        trigger_id,
        menu_id,
        test_id,
        side: MenuSide::Below,
        aria_label,
    });
    rsx! {
        div { "data-name": "ContextMenu", class: "contents",
            {children}
        }
    }
}

#[component]
pub fn ContextMenuTrigger(
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let Some(context) = try_use_context::<MenuContext>() else {
        return rsx! {};
    };
    let root = context.root.read().clone();
    let trigger_id = root.trigger_id.clone();
    let merged = tw_merge!("contents", class.as_deref().unwrap_or(""));
    rsx! {
        div {
            id: "{trigger_id}",
            // Focus returns here on close, so the region must accept focus
            // without joining the tab order.
            tabindex: "-1",
            "aria-haspopup": "menu",
            "data-name": "ContextMenuTrigger",
            "data-context-trigger": "{root.menu_id}",
            class: merged,
            oncontextmenu: move |event: MouseEvent| {
                event.prevent_default();
                let point = event.client_coordinates();
                request_open(
                    context,
                    Some(MenuPlacement::Point { x: point.x, y: point.y }),
                    Some(MenuFocusEdge::First),
                );
            },
            {children}
        }
    }
}

#[component]
pub fn ContextMenuContent(
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let merged = tw_merge!(
        "z-50 p-1 rounded-md border bg-card shadow-md w-[200px] fixed transition-all duration-200 data-[state=closed]:opacity-0 data-[state=closed]:scale-95 data-[state=open]:opacity-100 data-[state=open]:scale-100",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        MenuSurface {
            class: merged,
            name: "ContextMenuContent",
            target: "target__context",
            {children}
        }
    }
}

#[component]
pub fn ContextMenuItem(
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] test_id: Option<String>,
    #[props(into, optional)] title: Option<String>,
    #[props(default)] disabled: bool,
    onclick: Option<EventHandler<MouseEvent>>,
    onmouseenter: Option<EventHandler<MouseEvent>>,
    children: Element,
) -> Element {
    render_menu_item(
        "ContextMenuItem",
        class,
        disabled,
        MenuItemHooks { test_id, title },
        onclick,
        onmouseenter,
        children,
    )
}

#[component]
pub fn ContextMenuLabel(
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    render_menu_label("ContextMenuLabel", class, children)
}

#[component]
pub fn ContextMenuGroup(
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    render_menu_group("ContextMenuGroup", class, children)
}

#[component]
pub fn ContextMenuSeparator(#[props(into, optional)] class: Option<String>) -> Element {
    rsx! {
        Separator { class: class.unwrap_or_default() }
    }
}
