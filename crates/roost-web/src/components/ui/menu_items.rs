//! Item, label and group markup shared by `ui/dropdown_menu.rs` and
//! `ui/context_menu.rs`, with the Tailwind recipes kept in one place. Activating
//! an item closes the enclosing menu through `ui/menu_overlay.rs`.

use dioxus::prelude::*;
use tw_merge::tw_merge;

use super::menu_overlay::{MenuContext, request_close};

/// Optional DOM hooks on one item: a test id and a native tooltip title.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MenuItemHooks {
    pub test_id: Option<String>,
    pub title: Option<String>,
}

/// One `role="menuitem"` button. `name` is the rust-ui `data-name` of the calling
/// component. `onmouseenter` lets a caller keep its own highlighted row in step
/// with the pointer.
pub fn render_menu_item(
    name: &'static str,
    class: Option<String>,
    disabled: bool,
    hooks: MenuItemHooks,
    onclick: Option<EventHandler<MouseEvent>>,
    onmouseenter: Option<EventHandler<MouseEvent>>,
    children: Element,
) -> Element {
    let Some(context) = try_use_context::<MenuContext>() else {
        return rsx! {};
    };
    // Button-only additions (`text-left`, `disabled:*`) and the focus highlight
    // are needed because a focusable button replaces rust-ui's `li`; the keyboard
    // cursor is the focus ring.
    let merged = tw_merge!(
        "inline-flex gap-2 items-center w-full rounded-sm px-2 py-1.5 text-sm no-underline transition-colors duration-200 text-popover-foreground hover:bg-accent hover:text-accent-foreground [&_svg:not([class*='size-'])]:size-4 text-left focus:outline-hidden focus:bg-accent focus:text-accent-foreground disabled:pointer-events-none disabled:opacity-50",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        button {
            r#type: "button",
            role: "menuitem",
            tabindex: "-1",
            disabled,
            "aria-disabled": if disabled { "true" } else { "false" },
            "data-name": name,
            "data-testid": hooks.test_id,
            title: hooks.title,
            class: merged,
            onclick: move |event: MouseEvent| {
                if let Some(handler) = onclick {
                    handler.call(event);
                }
                request_close(context, true);
            },
            onmouseenter: move |event: MouseEvent| {
                if let Some(handler) = onmouseenter {
                    handler.call(event);
                }
            },
            {children}
        }
    }
}

pub fn render_menu_label(name: &'static str, class: Option<String>, children: Element) -> Element {
    let merged = tw_merge!(
        "px-2 py-1.5 text-sm font-medium data-inset:pl-8 mb-1",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        span { "data-name": name, class: merged, {children} }
    }
}

pub fn render_menu_group(name: &'static str, class: Option<String>, children: Element) -> Element {
    let merged = tw_merge!("group", class.as_deref().unwrap_or(""));
    rsx! {
        ul { role: "group", "data-name": name, class: merged, {children} }
    }
}
