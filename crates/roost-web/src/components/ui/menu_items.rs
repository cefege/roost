//! Item, label, group and separator markup shared by `ui/dropdown_menu.rs` and
//! `ui/context_menu.rs`, with the Tailwind recipes kept in one place. Activating
//! an item closes the enclosing menu through `ui/menu_overlay.rs`.

use dioxus::prelude::*;
use tw_merge::tw_merge;

use super::menu_overlay::{MenuContext, request_close};

pub fn render_menu_item(
    class: Option<String>,
    disabled: bool,
    onclick: Option<EventHandler<MouseEvent>>,
    children: Element,
) -> Element {
    let Some(context) = try_use_context::<MenuContext>() else {
        return rsx! {};
    };
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
            class: merged,
            onclick: move |event: MouseEvent| {
                if let Some(handler) = onclick {
                    handler.call(event);
                }
                request_close(context, true);
            },
            {children}
        }
    }
}

pub fn render_menu_label(class: Option<String>, children: Element) -> Element {
    let merged = tw_merge!(
        "px-2 py-1.5 text-sm font-medium data-[inset]:pl-8 mb-1",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        div { class: merged, {children} }
    }
}

pub fn render_menu_group(class: Option<String>, children: Element) -> Element {
    let merged = tw_merge!("group", class.as_deref().unwrap_or(""));
    rsx! {
        div { role: "group", class: merged, {children} }
    }
}

/// `ui::separator` does not exist in this crate, so the rule is drawn here
/// with the same border token the rest of the menu uses.
pub fn render_menu_separator(class: Option<String>) -> Element {
    let merged = tw_merge!("bg-border -mx-1 my-1 h-px", class.as_deref().unwrap_or(""));
    rsx! {
        div { role: "separator", class: merged }
    }
}
