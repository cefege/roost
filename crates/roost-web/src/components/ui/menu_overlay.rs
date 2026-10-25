//! Shared state behind the controlled `DropdownMenu` and `ContextMenu` roots:
//! the root context, open and close transitions, focus requests, keyboard
//! routing and placement. Used by `ui/dropdown_menu.rs`, `ui/context_menu.rs`
//! and `ui/menu_items.rs`; placement reuses `ui::anchored_panel` and focus and
//! key routing reuse `deck::deck_dom`.

use dioxus::prelude::*;

use crate::components::context_menu::{
    MenuFocusEdge, MenuFocusRequest, attempt_menu_focus, focus_menu_edge, use_floating_menu_dismiss,
};
use crate::components::deck::deck_dom::{focus_by_id, run_menu_keys};
use crate::components::ui::anchored_panel::anchored_placement_style;

/// The side of the trigger a dropdown opens on when both sides have room.
/// `Below` is the default; `Above` suits a trigger at the bottom of a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MenuSide {
    #[default]
    Below,
    Above,
}

impl MenuSide {
    /// Whether placement tries the space above the trigger first.
    pub fn prefers_up(self) -> bool {
        self == MenuSide::Above
    }
}

/// Where an open menu sits: beside its trigger, or at a pointer point.
#[derive(Debug, Clone, PartialEq)]
pub enum MenuPlacement {
    /// Longhand `top`/`bottom`/`left`/`right`/`max-height` from `anchored_panel`,
    /// written in the containing block's coordinates so a transformed ancestor
    /// does not shift the menu.
    Anchored(String),
    /// Viewport coordinates of the pointer.
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
    pub side: MenuSide,
    pub aria_label: Option<String>,
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
    // Only a data change is written: a write per render would re-render every
    // part on every render of the root. The stored `on_open_change` needs no
    // refresh because Dioxus repoints a re-rendered prop callback in place.
    let changed = {
        let current = context.root.peek();
        current.open != root.open
            || current.trigger_id != root.trigger_id
            || current.menu_id != root.menu_id
            || current.test_id != root.test_id
    };
    if changed {
        context.root.set(root);
    }
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

/// Inline placement for a placement. Both branches name all four edges, so
/// switching between them never leaves a stale edge set.
pub fn placement_style(placement: &MenuPlacement) -> String {
    match placement {
        MenuPlacement::Anchored(style) => style.clone(),
        MenuPlacement::Point { x, y } => {
            format!("top: {y}px; bottom: auto; right: auto; left: {x}px;")
        }
    }
}

/// Places a menu beside `trigger_id` on `side`, flipping to the other side and
/// aligning to whichever trigger edge keeps it inside the viewport. Falls back
/// to the origin when there is no layout to measure (off-wasm, or no trigger).
pub fn anchored_placement(trigger_id: &str, side: MenuSide) -> MenuPlacement {
    match anchored_placement_style(trigger_id, side.prefers_up(), true) {
        Some(style) => MenuPlacement::Anchored(style),
        None => MenuPlacement::Point { x: 0.0, y: 0.0 },
    }
}

/// The `role="menu"` panel shared by dropdown and context content. Renders
/// nothing while closed so the node is absent from the DOM, not merely hidden.
#[component]
pub fn MenuSurface(
    class: String,
    name: &'static str,
    target: &'static str,
    children: Element,
) -> Element {
    let Some(context) = try_use_context::<MenuContext>() else {
        return rsx! {};
    };
    if !context.root.read().open {
        return rsx! {};
    }
    rsx! {
        MenuPanel { class, name, target, {children} }
    }
}

/// Mounted only while open, so the document listeners it installs are removed
/// when the menu closes instead of staying live for the root's lifetime. It
/// mounts `data-state="closed"` and flips to `open` once attached, so rust-ui's
/// `data-[state=…]` transition runs on entrance.
#[component]
fn MenuPanel(
    class: String,
    name: &'static str,
    target: &'static str,
    children: Element,
) -> Element {
    let Some(context) = try_use_context::<MenuContext>() else {
        return rsx! {};
    };
    let root = context.root.read().clone();
    let style = placement_style(&context.placement.read());
    let mut shown = use_signal(|| false);
    let is_shown = *shown.read();
    let state = if is_shown { "open" } else { "closed" };
    let pointer_events = if is_shown { "auto" } else { "none" };
    let style_text = format!("pointer-events: {pointer_events}; {style}");
    use_floating_menu_dismiss(
        EventHandler::new(move |()| request_close(context, false)),
        Some(EventHandler::new(move |()| request_close(context, true))),
        vec![root.trigger_id.clone(), root.menu_id.clone()],
    );
    rsx! {
        div {
            id: "{root.menu_id}",
            role: "menu",
            "aria-labelledby": root.aria_label.is_none().then(|| root.trigger_id.clone()),
            "aria-label": root.aria_label.clone(),
            "data-testid": root.test_id.clone(),
            "data-name": name,
            "data-target": target,
            "data-state": state,
            tabindex: "-1",
            class,
            style: "{style_text}",
            onmounted: move |_| {
                attempt_pending_focus(context);
                shown.set(true);
            },
            onkeydown: move |event: KeyboardEvent| menu_keydown(context, &event),
            oncontextmenu: move |event: MouseEvent| event.prevent_default(),
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
