//! Controlled submenu parts for `DropdownMenu`.
//! The menu root owns lifecycle and item activation; this adapter adds unique
//! IDs, focus, viewport placement and dismissal around rust-ui's recipes.

use dioxus::prelude::*;
use icons::common::IconType;
use tw_merge::tw_merge;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;

use super::field::scoped_element_id;
use super::menu_overlay::{MenuContext, request_close};
use crate::components::context_menu::{
    MenuFocusEdge, attempt_menu_focus, focus_menu_edge, use_floating_menu_dismiss,
};
use crate::components::deck::deck_dom::run_menu_keys;
use crate::components::layout::portal::Portal;
use crate::components::ui::icon::Icon;

mod position;
use position::place_panel;

/// Preferred placement; the panel flips when the preferred side does not fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DropdownMenuSubSide {
    #[default]
    Right,
    Left,
    Above,
    Below,
}

#[derive(Clone)]
struct SubmenuRoot {
    open: bool,
    on_open_change: EventHandler<bool>,
    trigger_id: String,
    content_id: String,
    side: DropdownMenuSubSide,
    focus_on_open: bool,
    parent: MenuContext,
}

#[component]
pub fn DropdownMenuSub(
    open: bool,
    on_open_change: EventHandler<bool>,
    #[props(default)] side: DropdownMenuSubSide,
    children: Element,
) -> Element {
    let Some(parent) = try_use_context::<MenuContext>() else {
        return rsx! {};
    };
    let trigger_id = use_hook(|| scoped_element_id("dropdown-menu-sub-trigger"));
    let content_id = use_hook(|| scoped_element_id("dropdown-menu-sub-content"));
    let mut context = use_context_provider(|| {
        Signal::new(SubmenuRoot {
            open,
            on_open_change,
            trigger_id: trigger_id.clone(),
            content_id: content_id.clone(),
            side,
            focus_on_open: false,
            parent,
        })
    });
    let (props_changed, focus_on_open) = {
        let current = context.peek();
        (
            current.open != open
                || current.trigger_id != trigger_id
                || current.content_id != content_id
                || current.side != side
                || current.on_open_change != on_open_change
                || current.parent.root != parent.root,
            current.open == open && current.focus_on_open,
        )
    };
    if props_changed {
        context.set(SubmenuRoot {
            open,
            on_open_change,
            trigger_id,
            content_id,
            side,
            focus_on_open,
            parent,
        });
    }
    rsx! {
        li {
            "data-name": "DropdownMenuSub",
            class: "dropdown__menu_sub_trigger relative inline-flex relative gap-2 items-center py-1.5 px-2 w-full text-sm no-underline rounded-sm transition-colors duration-200 cursor-pointer text-popover-foreground [&_svg:not([class*='size-'])]:size-4 hover:bg-accent hover:text-accent-foreground",
            {children}
        }
    }
}

#[component]
pub fn DropdownMenuSubTrigger(
    #[props(default)] disabled: bool,
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] test_id: Option<String>,
    children: Element,
) -> Element {
    let Some(mut context) = try_use_context::<Signal<SubmenuRoot>>() else {
        return rsx! {};
    };
    let root = context.read().clone();
    let merged = tw_merge!(
        "flex items-center justify-between w-full",
        class.as_deref().unwrap_or("")
    );
    let owner_id = root.parent.root.read().menu_id.clone();
    let trigger_classes = "flex items-center justify-between w-full focus:outline-hidden focus:bg-accent focus:text-accent-foreground disabled:pointer-events-none disabled:opacity-50";
    rsx! {
        button {
            id: "{root.trigger_id}",
            r#type: "button",
            role: "menuitem",
            tabindex: "-1",
            "aria-haspopup": "menu",
            "aria-expanded": if root.open { "true" } else { "false" },
            "aria-controls": root.open.then(|| root.content_id.clone()),
            "data-menu-owner": "{owner_id}",
            "data-name": "DropdownMenuSubTrigger",
            "data-testid": test_id,
            disabled,
            class: tw_merge!(trigger_classes, merged.as_str()),
            onmouseenter: move |_| {
                if disabled { return; }
                let mut next_root = context.read().clone();
                if !next_root.open {
                    next_root.open = true;
                    next_root.focus_on_open = false;
                    next_root.on_open_change.call(true);
                    context.set(next_root);
                }
            },
            onclick: move |_| {
                if disabled || context.peek().open { return; }
                let mut next_root = context.peek().clone();
                next_root.open = true;
                next_root.focus_on_open = false;
                next_root.on_open_change.call(true);
                context.set(next_root);
            },
            onkeydown: move |event: KeyboardEvent| {
                if disabled { return; }
                let is_open_key = match event.key() {
                    Key::Named(NamedKey::ArrowRight | NamedKey::Enter) => true,
                    Key::Character(character) => character == " ",
                    _ => false,
                };
                if is_open_key {
                    event.prevent_default();
                    event.stop_propagation();
                    let mut next_root = context.peek().clone();
                    let was_open = next_root.open;
                    next_root.open = true;
                    next_root.focus_on_open = !was_open;
                    if was_open {
                        focus_menu_edge(&next_root.content_id, MenuFocusEdge::First);
                    }
                    next_root.on_open_change.call(true);
                    context.set(next_root);
                }
            },
            span { class: "{merged}",
                span { class: "flex gap-2 items-center", {children} }
                Icon { icon: IconType::ChevronRight, class: "opacity-70 size-4" }
            }
        }
    }
}

#[component]
pub fn DropdownMenuSubContent(
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] test_id: Option<String>,
    children: Element,
) -> Element {
    let Some(context) = try_use_context::<Signal<SubmenuRoot>>() else {
        return rsx! {};
    };
    if !context.read().open {
        return rsx! {};
    }
    rsx! { SubmenuPanel { context, class, test_id, {children} } }
}

#[component]
fn SubmenuPanel(
    context: Signal<SubmenuRoot>,
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] test_id: Option<String>,
    children: Element,
) -> Element {
    let root = context.read().clone();
    let style = use_signal(String::new);
    let classes = tw_merge!(
        "dropdown__menu_sub_content rounded-md border bg-card shadow-lg p-1 fixed z-[100] min-w-[160px] opacity-100 visible translate-x-0 transition-all duration-200 ease-out pointer-events-auto",
        class.as_deref().unwrap_or("")
    );
    let close_outside = EventHandler::new(move |()| {
        let mut next = context.read().clone();
        next.open = false;
        next.focus_on_open = false;
        next.on_open_change.call(false);
        context.set(next);
    });
    let close_escape = EventHandler::new(move |()| {
        let root = context.read().clone();
        root.on_open_change.call(false);
        let mut next = root;
        next.open = false;
        next.focus_on_open = false;
        context.set(next);
        focus_trigger(&context);
    });
    use_floating_menu_dismiss(
        close_outside,
        Some(close_escape),
        vec![root.trigger_id.clone(), root.content_id.clone()],
    );
    let owner_id = root.parent.root.read().menu_id.clone();
    let focus_on_open = root.focus_on_open;
    let content_id = root.content_id.clone();
    let trigger_id = root.trigger_id.clone();
    let side = root.side;
    rsx! {
        Portal {
            div {
                id: "{content_id}",
                role: "menu",
                tabindex: "-1",
                "aria-labelledby": "{trigger_id}",
                "aria-orientation": "vertical",
                "data-name": "DropdownMenuSubContent",
                "data-menu-owner": "{owner_id}",
                "data-testid": test_id,
                class: classes,
                style: "{style}",
                onmounted: move |event: MountedEvent| async move {
                    place_panel(context, event.data(), style, side).await;
                    if focus_on_open {
                        attempt_menu_focus(&focus_menu_edge(&context.peek().content_id, MenuFocusEdge::First));
                    }
                },
                onkeydown: move |event: KeyboardEvent| {
                    let key = event.key();
                    if key == Key::Named(NamedKey::Escape) {
                        event.stop_propagation();
                    }
                    if key == Key::Named(NamedKey::ArrowLeft) {
                        event.prevent_default();
                        event.stop_propagation();
                        let mut next = context.read().clone();
                        next.open = false;
                        next.focus_on_open = false;
                        next.on_open_change.call(false);
                        context.set(next);
                        focus_trigger(&context);
                    } else {
                        if matches!(
                            key,
                            Key::Named(NamedKey::ArrowUp)
                                | Key::Named(NamedKey::ArrowDown)
                                | Key::Named(NamedKey::Home)
                                | Key::Named(NamedKey::End)
                        ) {
                            event.stop_propagation();
                        }
                        let escape_context = context;
                        let tab_context = context;
                        run_menu_keys(
                            &event,
                            &content_id,
                            move || close_submenu(escape_context, true),
                            move || {
                                close_submenu(tab_context, false);
                                request_close(context.read().parent, false);
                            },
                        );
                    }
                },
                {children}
            }
        }
    }
}

#[component]
pub fn DropdownMenuSubItem(
    #[props(default)] disabled: bool,
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] test_id: Option<String>,
    on_select: Option<EventHandler<MouseEvent>>,
    children: Element,
) -> Element {
    let Some(mut context) = try_use_context::<Signal<SubmenuRoot>>() else {
        return rsx! {};
    };
    let classes = tw_merge!(
        "inline-flex gap-2 items-center w-full rounded-sm px-3 py-2 text-sm transition-all duration-150 ease text-popover-foreground hover:bg-accent hover:text-accent-foreground cursor-pointer hover:translate-x-[2px] focus:outline-hidden focus:bg-accent focus:text-accent-foreground disabled:pointer-events-none disabled:opacity-50",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        button {
            r#type: "button",
            role: "menuitem",
            tabindex: "-1",
            disabled,
            "aria-disabled": if disabled { "true" } else { "false" },
            "data-name": "DropdownMenuSubItem",
            "data-testid": test_id,
            class: classes,
            onclick: move |event: MouseEvent| {
                if disabled { return; }
                if let Some(handler) = on_select { handler.call(event); }
                let root = context.read().clone();
                let parent = root.parent;
                root.on_open_change.call(false);
                let mut next = root;
                next.open = false;
                next.focus_on_open = false;
                context.set(next);
                request_close(parent, true);
            },
            {children}
        }
    }
}

fn close_submenu(mut context: Signal<SubmenuRoot>, restore_focus: bool) {
    let mut root = context.read().clone();
    root.on_open_change.call(false);
    root.open = false;
    root.focus_on_open = false;
    context.set(root);
    if restore_focus {
        focus_trigger(&context);
    }
}
fn focus_trigger(context: &Signal<SubmenuRoot>) {
    #[cfg(target_arch = "wasm32")]
    if let Some(element) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id(&context.read().trigger_id))
        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
    {
        let _ = element.focus();
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = context;
}
