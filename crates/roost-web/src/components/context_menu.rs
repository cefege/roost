//! Shared floating-menu plumbing: roving keyboard focus and the focus request a
//! menu hands its mount, right-anchored placement arithmetic, and outside-click
//! and Escape dismissal. The rendered menus are `components::ui::{dropdown_menu,
//! context_menu}`. The key decision (`menu_key_action`) is pure; `dom` holds the
//! focus and listener adapters. Mobile bottom sheets are separate.

#[cfg(target_arch = "wasm32")]
mod dom;
pub mod terminal_menu;

use std::cell::Cell;
use std::rc::Rc;

use dioxus::prelude::*;

#[cfg(target_arch = "wasm32")]
pub use dom::{attempt_menu_focus, focus_menu_edge, run_menu_key};

/// Focus a menu edge where there is no document to focus in.
///
/// A native build has no rendered menu, so the roving-focus step has nothing to
/// do. It is a real answer rather than a refusal: every caller asks the same
/// question on both targets, and gating the CALL instead would put a
/// `#[cfg]` block in every menu that opens on a key.
#[cfg(not(target_arch = "wasm32"))]
pub fn focus_menu_edge(menu_id: &str, edge: MenuFocusEdge) -> MenuFocusRequest {
    MenuFocusRequest::new(menu_id, edge)
}

/// Attempt a focus request where there is no document; see `focus_menu_edge`.
#[cfg(not(target_arch = "wasm32"))]
pub fn attempt_menu_focus(_request: &MenuFocusRequest) {}

/// One request to focus a menu's edge once the menu mounts, as
/// `focus_menu_edge` returns it. Clones share the request.
///
/// Dropping a handle does not cancel it, so a caller that never keeps one
/// still gets every retry. A menu that keeps it cancels it on close and on its
/// next request: a stale retry would pull focus off the item the reader's
/// next key already moved to.
#[derive(Debug, Clone)]
pub struct MenuFocusRequest {
    menu_id: Rc<str>,
    edge: MenuFocusEdge,
    pending: Rc<Cell<bool>>,
}

impl MenuFocusRequest {
    /// A pending request for menu `menu_id`'s `edge`.
    pub fn new(menu_id: &str, edge: MenuFocusEdge) -> Self {
        Self {
            menu_id: Rc::from(menu_id),
            edge,
            pending: Rc::new(Cell::new(true)),
        }
    }

    /// The menu element's id.
    pub fn menu_id(&self) -> &str {
        &self.menu_id
    }

    /// The item the request focuses.
    pub fn edge(&self) -> MenuFocusEdge {
        self.edge
    }

    /// Whether a later attempt may still move focus.
    pub fn is_pending(&self) -> bool {
        self.pending.get()
    }

    /// Stop every later attempt. A request whose focus landed, or whose
    /// retries ran out, cancels itself.
    pub fn cancel(&self) {
        self.pending.set(false);
    }
}

/// Which end of a menu receives focus when it opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuFocusEdge {
    /// The first enabled item.
    First,
    /// The last enabled item.
    Last,
}

/// What a key pressed inside a menu does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuKeyAction {
    /// Close the menu (Escape); the event is consumed.
    Escape,
    /// Let focus leave natively, then run the menu's Tab handler.
    Tab,
    /// Move focus to the enabled item at this index; the event is consumed.
    Focus(usize),
    /// Click the focused item; the event is consumed.
    Activate(usize),
    /// Not the menu's key.
    Ignore,
}

/// Roving focus over `count` enabled items with `current` focused: arrows wrap,
/// Home/End jump, Enter/Space click the focused item.
pub fn menu_key_action(key: &str, current: Option<usize>, count: usize) -> MenuKeyAction {
    match key {
        "Escape" => return MenuKeyAction::Escape,
        "Tab" => return MenuKeyAction::Tab,
        _ => {}
    }
    if count == 0 {
        return MenuKeyAction::Ignore;
    }
    let last = count - 1;
    match (key, current) {
        ("ArrowDown", None) | ("Home", _) => MenuKeyAction::Focus(0),
        ("ArrowDown", Some(index)) => MenuKeyAction::Focus((index + 1) % count),
        ("ArrowUp", None) | ("End", _) => MenuKeyAction::Focus(last),
        ("ArrowUp", Some(index)) => MenuKeyAction::Focus(if index == 0 { last } else { index - 1 }),
        ("Enter" | " ", Some(index)) => MenuKeyAction::Activate(index),
        _ => MenuKeyAction::Ignore,
    }
}

/// Outside-click and Escape dismissal for the calling menu's lifetime. A click
/// closes UNLESS it lands inside an element whose id is in `within_ids` (the
/// trigger, the menu); item clicks close explicitly.
pub fn use_floating_menu_dismiss(
    on_close: EventHandler<()>,
    on_escape: Option<EventHandler<()>>,
    within_ids: Vec<String>,
) {
    #[cfg(target_arch = "wasm32")]
    {
        let listeners = use_hook(move || {
            std::rc::Rc::new(dom::DismissListeners::install(
                on_close, on_escape, within_ids,
            ))
        });
        use_drop(move || listeners.remove());
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (on_close, on_escape, within_ids);
}
