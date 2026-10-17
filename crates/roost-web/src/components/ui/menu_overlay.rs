//! Shared state behind the controlled `DropdownMenu` and `ContextMenu` roots:
//! the root context, open and close transitions, focus requests, keyboard
//! routing and placement. Used by `ui/dropdown_menu.rs`, `ui/context_menu.rs`
//! and `ui/menu_items.rs`; focus and key routing reuse `deck::deck_dom`.

use dioxus::prelude::*;

#[cfg(target_arch = "wasm32")]
use crate::components::context_menu::anchored_menu_position;
use crate::components::context_menu::{
    AnchoredMenuPos, MenuFocusEdge, MenuFocusRequest, attempt_menu_focus, focus_menu_edge,
    use_floating_menu_dismiss,
};
use crate::components::deck::deck_dom::{focus_by_id, run_menu_keys};

/// Where an open menu sits: under a trigger's right edge, or at a pointer point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MenuPlacement {
    Anchored(AnchoredMenuPos),
    Point { x: f64, y: f64 },
}

/// What a root writes on every render so its parts can read the controlled state.
#[derive(Clone, Debug)]
pub struct MenuRoot {
    pub open: bool,
    pub on_open_change: EventHandler<bool>,
    pub trigger_id: String,
    pub menu_id: String,
    pub test_id: Option<String>,
}

/// Signals shared by one root and its parts. `Copy` because every field is a signal.
#[derive(Clone, Copy, Debug)]
pub struct MenuContext {
    pub root: Signal<MenuRoot>,
    pub placement: Signal<MenuPlacement>,
    pub focus_request: Signal<Option<MenuFocusRequest>>,
}

/// Installs the menu context for a root. Each render refreshes the controlled
/// state; the outside-click and Escape listeners are mounted by the open panel.
pub fn provide_menu_root(root: MenuRoot) -> MenuContext {
    let mut context = use_context_provider(|| MenuContext {
        root: Signal::new(root.clone()),
        placement: Signal::new(MenuPlacement::Point { x: 0.0, y: 0.0 }),
        focus_request: Signal::new(None),
    });
    context.root.set(root);
    context
}

/// Opens the menu, optionally repositioning it and moving focus to an item.
/// An already-open menu is repositioned and refocused instead of re-announced.
pub fn request_open(
    mut context: MenuContext,
    placement: Option<MenuPlacement>,
    edge: Option<MenuFocusEdge>,
) {
    let root = context.root.peek().clone();
    if let Some(placement) = placement {
        context.placement.set(placement);
    }
    if let Some(edge) = edge {
        cancel_pending_focus(context);
        context
            .focus_request
            .set(Some(focus_menu_edge(&root.menu_id, edge)));
    }
    if root.open {
        attempt_pending_focus(context);
        return;
    }
    tracing::debug!(target: "ui", menu_id = %root.menu_id, "menu opened");
    root.on_open_change.call(true);
}

/// Closes the menu and cancels any pending focus move. Focus returns to the
/// trigger only for deliberate dismissals (Escape, item activation); an outside
/// click leaves focus where the user aimed it.
pub fn request_close(context: MenuContext, restore_focus: bool) {
    let root = context.root.peek().clone();
    if !root.open {
        return;
    }
    cancel_pending_focus(context);
    tracing::debug!(target: "ui", menu_id = %root.menu_id, restore_focus, "menu closed");
    root.on_open_change.call(false);
    if restore_focus {
        focus_by_id(&root.trigger_id);
    }
}

/// Longhand `top`/`left`/`right` for a placement. Every branch names all three,
/// so switching placement never leaves a stale edge set.
pub fn placement_style(placement: MenuPlacement) -> String {
    match placement {
        MenuPlacement::Anchored(position) => {
            format!("top: {}px; left: auto; right: {}px;", position.y, position.right)
        }
        MenuPlacement::Point { x, y } => format!("top: {y}px; right: auto; left: {x}px;"),
    }
}

/// Anchors a menu under the trigger's right edge. Off-wasm there is no layout,
/// so the origin stands in and the menu is not positioned against a real trigger.
#[cfg(target_arch = "wasm32")]
pub fn anchored_placement(trigger_id: &str) -> MenuPlacement {
    let trigger = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id(trigger_id));
    let position = match trigger {
        Some(element) => anchored_menu_position(&element),
        None => AnchoredMenuPos { right: 0.0, y: 0.0 },
    };
    MenuPlacement::Anchored(position)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn anchored_placement(_trigger_id: &str) -> MenuPlacement {
    MenuPlacement::Anchored(AnchoredMenuPos { right: 0.0, y: 0.0 })
}

/// The `role="menu"` panel shared by dropdown and context content. Renders
/// nothing while closed so the node is absent from the DOM, not merely hidden.
#[component]
pub fn MenuSurface(class: String, children: Element) -> Element {
    let Some(context) = try_use_context::<MenuContext>() else {
        return rsx! {};
    };
    if !context.root.read().open {
        return rsx! {};
    }
    rsx! {
        MenuPanel { class, {children} }
    }
}

/// Mounted only while open, so the document listeners it installs are removed
/// when the menu closes instead of staying live for the root's lifetime.
#[component]
fn MenuPanel(class: String, children: Element) -> Element {
    let Some(context) = try_use_context::<MenuContext>() else {
        return rsx! {};
    };
    let root = context.root.read().clone();
    let style = placement_style(*context.placement.read());
    use_floating_menu_dismiss(
        EventHandler::new(move |()| request_close(context, false)),
        Some(EventHandler::new(move |()| request_close(context, true))),
        vec![root.trigger_id.clone(), root.menu_id.clone()],
    );
    rsx! {
        div {
            id: "{root.menu_id}",
            role: "menu",
            "aria-labelledby": "{root.trigger_id}",
            "data-testid": root.test_id.clone(),
            tabindex: "-1",
            class,
            style: "{style}",
            onmounted: move |_| attempt_pending_focus(context),
            onkeydown: move |event: KeyboardEvent| menu_keydown(context, &event),
            {children}
        }
    }
}

fn cancel_pending_focus(mut context: MenuContext) {
    let pending = context.focus_request.peek().clone();
    if let Some(request) = pending {
        request.cancel();
    }
    context.focus_request.set(None);
}

/// Called from the menu's mount hook and from a reposition while already open.
/// `attempt_menu_focus` retries by frame until the node exists, so a request
/// made before mount still lands.
fn attempt_pending_focus(context: MenuContext) {
    let pending = context.focus_request.peek().clone();
    if let Some(request) = pending {
        attempt_menu_focus(&request);
    }
}

/// Escape and activation restore focus to the trigger; Tab does not, because
/// the browser's own Tab step already picks the next focus target.
fn menu_keydown(context: MenuContext, event: &KeyboardEvent) {
    let menu_id = context.root.peek().menu_id.clone();
    run_menu_keys(
        event,
        &menu_id,
        move || request_close(context, true),
        move || request_close(context, false),
    );
}
