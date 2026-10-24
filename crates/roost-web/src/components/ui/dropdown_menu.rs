//! Port of rust-ui `dropdown_menu`: a controlled root that takes `open` and
//! reports changes through `on_open_change`, a trigger button, and a
//! trigger-anchored `role="menu"` panel. `side` picks the preferred side of
//! the trigger. Shared state and items live in `ui/menu_overlay.rs` and
//! `ui/menu_items.rs`.

use dioxus::prelude::*;
use tw_merge::tw_merge;

use super::menu_items::{MenuItemHooks, render_menu_group, render_menu_item, render_menu_label};
use super::menu_overlay::{
    MenuContext, MenuRoot, MenuSide, MenuSurface, anchored_placement, provide_menu_root,
    request_close, request_open,
};
use super::separator::Separator;
use crate::components::context_menu::MenuFocusEdge;
use crate::components::ui::field::scoped_element_id;

/// `trigger_id` and `menu_id` exist for callers whose trigger id is already
/// pinned by tests or by another component's focus logic; otherwise generated.
#[component]
pub fn DropdownMenu(
    open: bool,
    on_open_change: EventHandler<bool>,
    #[props(into, optional)] test_id: Option<String>,
    #[props(default)] side: MenuSide,
    #[props(into, optional)] trigger_id: Option<String>,
    #[props(into, optional)] menu_id: Option<String>,
    children: Element,
) -> Element {
    let trigger_id =
        use_hook(|| trigger_id.unwrap_or_else(|| scoped_element_id("dropdown-menu-trigger")));
    let menu_id = use_hook(|| menu_id.unwrap_or_else(|| scoped_element_id("dropdown-menu")));
    provide_menu_root(MenuRoot {
        open,
        on_open_change,
        trigger_id,
        menu_id,
        test_id,
        side,
        aria_label: None,
    });
    rsx! {
        div { "data-name": "DropdownMenu",
            {children}
        }
    }
}

/// `on_key` runs before the built-in ArrowDown/ArrowUp/Escape handling and
/// returns `true` when it has handled the key, which skips the built-in pass.
#[component]
pub fn DropdownMenuTrigger(
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] test_id: Option<String>,
    #[props(into, optional)] aria_label: Option<String>,
    #[props(optional)] on_key: Option<Callback<KeyboardEvent, bool>>,
    #[props(default)] disabled: bool,
    /// Drops the bordered input look for a control that sits inside a toolbar row.
    #[props(default)]
    ghost: bool,
    children: Element,
) -> Element {
    let Some(context) = try_use_context::<MenuContext>() else {
        return rsx! {};
    };
    let root = context.root.read().clone();
    let trigger_id = root.trigger_id.clone();
    let side = root.side;
    let click_trigger_id = trigger_id.clone();
    let keydown_trigger_id = trigger_id.clone();
    let surface = if ghost {
        "hover:bg-accent hover:text-accent-foreground"
    } else {
        "border bg-background border-input hover:bg-accent hover:text-accent-foreground"
    };
    let merged = tw_merge!(
        "px-4 py-2 h-9 inline-flex justify-center items-center text-sm font-medium whitespace-nowrap rounded-md transition-colors w-fit focus:outline-none focus:ring-1 focus:ring-ring focus-visible:outline-hidden focus-visible:ring-1 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50 [&_svg:not([class*='size-'])]:size-4",
        surface,
        class.as_deref().unwrap_or("")
    );
    rsx! {
        button {
            id: "{trigger_id}",
            r#type: "button",
            tabindex: "0",
            "data-name": "DropdownMenuTrigger",
            "data-dropdown-trigger": "{root.menu_id}",
            "aria-haspopup": "menu",
            "aria-label": aria_label,
            "aria-expanded": if root.open { "true" } else { "false" },
            "aria-controls": root.open.then(|| root.menu_id.clone()),
            "data-testid": test_id,
            disabled,
            class: merged,
            onclick: move |_| {
                if context.root.peek().open {
                    request_close(context, true);
                } else {
                    request_open(context, Some(anchored_placement(&click_trigger_id, side)), None);
                }
            },
            onkeydown: move |event: KeyboardEvent| {
                if let Some(handler) = on_key
                    && handler.call(event.clone())
                {
                    return;
                }
                let open = context.root.peek().open;
                match event.key() {
                    Key::Named(NamedKey::ArrowDown) | Key::Named(NamedKey::ArrowUp) => {
                        event.prevent_default();
                        event.stop_propagation();
                        let edge = if event.key() == Key::Named(NamedKey::ArrowDown) {
                            MenuFocusEdge::First
                        } else {
                            MenuFocusEdge::Last
                        };
                        request_open(
                            context,
                            Some(anchored_placement(&keydown_trigger_id, side)),
                            Some(edge),
                        );
                    }
                    Key::Named(NamedKey::Escape) if open => {
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
pub fn DropdownMenuContent(
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let merged = tw_merge!(
        "z-50 p-1 rounded-md border bg-card shadow-md h-fit fixed w-[180px] transition-all duration-200 data-[state=closed]:opacity-0 data-[state=closed]:scale-95 data-[state=open]:opacity-100 data-[state=open]:scale-100",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        MenuSurface {
            class: merged,
            name: "DropdownMenuContent",
            target: "target__dropdown",
            {children}
        }
    }
}

#[component]
pub fn DropdownMenuItem(
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] test_id: Option<String>,
    #[props(into, optional)] title: Option<String>,
    #[props(default)] disabled: bool,
    onclick: Option<EventHandler<MouseEvent>>,
    onmouseenter: Option<EventHandler<MouseEvent>>,
    children: Element,
) -> Element {
    render_menu_item(
        "DropdownMenuItem",
        class,
        disabled,
        MenuItemHooks { test_id, title },
        onclick,
        onmouseenter,
        children,
    )
}

#[component]
pub fn DropdownMenuLabel(
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    render_menu_label("DropdownMenuLabel", class, children)
}

#[component]
pub fn DropdownMenuGroup(
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    render_menu_group("DropdownMenuGroup", class, children)
}

#[component]
pub fn DropdownMenuSeparator(#[props(into, optional)] class: Option<String>) -> Element {
    rsx! {
        Separator { class: class.unwrap_or_default() }
    }
}
