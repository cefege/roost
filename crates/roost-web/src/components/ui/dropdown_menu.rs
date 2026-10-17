//! Port of rust-ui `dropdown_menu`: a controlled root that takes `open` and
//! reports changes through `on_open_change`, a trigger button, and a
//! right-anchored `role="menu"` panel. Shared state and items live in
//! `ui/menu_overlay.rs` and `ui/menu_items.rs`.

use dioxus::prelude::*;
use tw_merge::tw_merge;

use super::menu_items::{
    render_menu_group, render_menu_item, render_menu_label, render_menu_separator,
};
use super::menu_overlay::{
    MenuContext, MenuRoot, MenuSurface, anchored_placement, provide_menu_root, request_close,
    request_open,
};
use crate::components::context_menu::MenuFocusEdge;
use crate::components::ui::field::scoped_element_id;

#[component]
pub fn DropdownMenu(
    open: bool,
    on_open_change: EventHandler<bool>,
    #[props(into, optional)] test_id: Option<String>,
    children: Element,
) -> Element {
    let trigger_id = use_hook(|| scoped_element_id("dropdown-menu-trigger"));
    let menu_id = use_hook(|| scoped_element_id("dropdown-menu"));
    provide_menu_root(MenuRoot {
        open,
        on_open_change,
        trigger_id,
        menu_id,
        test_id,
    });
    rsx! {
        {children}
    }
}

#[component]
pub fn DropdownMenuTrigger(
    #[props(into, optional)] class: Option<String>,
    #[props(default)] disabled: bool,
    children: Element,
) -> Element {
    let Some(context) = try_use_context::<MenuContext>() else {
        return rsx! {};
    };
    let root = context.root.read().clone();
    let trigger_id = root.trigger_id.clone();
    let click_trigger_id = trigger_id.clone();
    let keydown_trigger_id = trigger_id.clone();
    let merged = tw_merge!(
        "px-4 py-2 h-9 inline-flex justify-center items-center text-sm font-medium whitespace-nowrap rounded-md transition-colors w-fit focus:outline-none focus:ring-1 focus:ring-ring focus-visible:outline-hidden focus-visible:ring-1 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50 [&_svg:not([class*='size-'])]:size-4 border bg-background border-input hover:bg-accent hover:text-accent-foreground",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        button {
            id: "{trigger_id}",
            r#type: "button",
            "aria-haspopup": "menu",
            "aria-expanded": if root.open { "true" } else { "false" },
            "aria-controls": root.open.then(|| root.menu_id.clone()),
            disabled,
            class: merged,
            onclick: move |_| {
                if context.root.peek().open {
                    request_close(context, true);
                } else {
                    request_open(context, Some(anchored_placement(&click_trigger_id)), None);
                }
            },
            onkeydown: move |event: KeyboardEvent| {
                let open = context.root.peek().open;
                match event.key() {
                    Key::ArrowDown | Key::ArrowUp => {
                        event.prevent_default();
                        let edge = if event.key() == Key::ArrowDown {
                            MenuFocusEdge::First
                        } else {
                            MenuFocusEdge::Last
                        };
                        request_open(
                            context,
                            Some(anchored_placement(&keydown_trigger_id)),
                            Some(edge),
                        );
                    }
                    Key::Escape if open => {
                        event.prevent_default();
                        request_close(context, true);
                    }
                    _ => {}
                }
            },
            {children}
        }
    }
}

#[component]
pub fn DropdownMenuContent(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged = tw_merge!(
        "z-50 p-1 rounded-md border bg-popover text-popover-foreground shadow-md h-fit fixed w-44 transition-all duration-200",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        MenuSurface { class: merged, {children} }
    }
}

#[component]
pub fn DropdownMenuItem(
    #[props(into, optional)] class: Option<String>,
    #[props(default)] disabled: bool,
    onclick: Option<EventHandler<MouseEvent>>,
    children: Element,
) -> Element {
    render_menu_item(class, disabled, onclick, children)
}

#[component]
pub fn DropdownMenuLabel(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    render_menu_label(class, children)
}

#[component]
pub fn DropdownMenuGroup(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    render_menu_group(class, children)
}

#[component]
pub fn DropdownMenuSeparator(#[props(into, optional)] class: Option<String>) -> Element {
    render_menu_separator(class)
}
