//! Shared floating-menu chrome: the surface style, the item and separator rows,
//! right-anchored placement, roving keyboard focus and outside-click/Escape
//! dismissal. Ports `apps/web/src/components/contextMenuPrimitives.tsx`; composed
//! by the terminal, sidebar-row, pane, arrange and compact workspace menus.
//!
//! The key decision (`menu_key_action`) is pure; `dom` holds the focus and
//! listener adapters. Mobile bottom sheets are separate.

#[cfg(target_arch = "wasm32")]
mod dom;
pub mod terminal_menu;

use std::cell::Cell;
use std::rc::Rc;

use dioxus::prelude::*;

#[cfg(target_arch = "wasm32")]
pub use dom::{anchored_menu_position, attempt_menu_focus, focus_menu_edge, run_menu_key};

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
/// next request, as v2's `cancelPendingFocus` did: a stale retry would pull
/// focus off the item the reader's next key already moved to.
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

/// The terminal menu's stacking level; the sidebar-row menu passes 100 to sit
/// above its click-away scrim.
pub const DEFAULT_MENU_Z_INDEX: u32 = 40;

/// The canonical floating-menu surface at viewport (`x`, `y`).
pub fn ctx_menu_surface_style(x: f64, y: f64, z_index: u32) -> String {
    format!(
        "position: fixed; left: {x}px; top: {y}px; {}",
        surface_chrome(z_index, "180px")
    )
}

/// A menu anchored under a trigger with right edges aligned: `right` is the
/// offset from the viewport's right edge, so a shrink-fit menu grows leftward.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnchoredMenuPos {
    /// Offset from the viewport's right edge.
    pub right: f64,
    /// Top edge.
    pub y: f64,
}

/// The smallest gap kept between a right-anchored menu and the viewport edge.
pub const ANCHOR_EDGE_GAP_PX: f64 = 6.0;
/// The gap between a trigger's bottom and its menu.
pub const ANCHOR_DROP_PX: f64 = 4.0;

/// Where a menu anchored under a trigger sits, from the trigger's right and
/// bottom edges and the viewport width.
pub fn anchored_menu_pos(
    trigger_right: f64,
    trigger_bottom: f64,
    viewport_width: f64,
) -> AnchoredMenuPos {
    AnchoredMenuPos {
        right: (viewport_width - trigger_right).max(ANCHOR_EDGE_GAP_PX),
        y: trigger_bottom + ANCHOR_DROP_PX,
    }
}

/// The right-anchored surface: no `left` (a stale left plus this right would
/// stretch the box full width), the trigger's `right`, then `extra` overrides.
pub fn anchored_menu_surface_style(
    pos: AnchoredMenuPos,
    min_width: &str,
    z_index: Option<u32>,
    extra: &str,
) -> String {
    format!(
        "position: fixed; top: {}px; right: {}px; {} {extra}",
        pos.y,
        pos.right,
        surface_chrome(z_index.unwrap_or(DEFAULT_MENU_Z_INDEX), min_width),
    )
}

fn surface_chrome(z_index: u32, min_width: &str) -> String {
    format!(
        "z-index: {z_index}; min-width: {min_width}; \
         background: var(--md-surface-container-high); \
         border: 1px solid var(--md-outline-variant); \
         border-radius: var(--md-shape-sm); box-shadow: var(--md-elev-3); \
         padding: var(--md-space-1); user-select: none; color: var(--text-hi); \
         font-size: var(--md-body-s-size);"
    )
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

/// A thin rule between menu groups.
#[component]
pub fn CtxMenuSeparator() -> Element {
    rsx! {
        div {
            role: "separator",
            style: "border: 0 solid var(--md-outline-variant); border-block-start-width: var(--workbench-border-width); margin: var(--md-space-1) 0;",
        }
    }
}

/// One native, programmatically focusable menu row. Items stay out of the tab
/// order; `disabled` removes an action from roving focus and from clicks.
#[component]
pub fn CtxMenuItem(
    testid: String,
    onclick: EventHandler<MouseEvent>,
    #[props(default)] danger: bool,
    #[props(default)] disabled: bool,
    #[props(default)] selected: bool,
    #[props(default)] highlighted: bool,
    #[props(default)] class: Option<String>,
    #[props(default)] title: Option<String>,
    #[props(default)] onfocus: Option<EventHandler<FocusEvent>>,
    #[props(default)] onmouseenter: Option<EventHandler<MouseEvent>>,
    children: Element,
) -> Element {
    let class = menu_item_class(danger, class.as_deref());
    rsx! {
        button {
            r#type: "button",
            "data-testid": testid,
            class,
            role: "menuitem",
            tabindex: "-1",
            disabled,
            "aria-disabled": disabled.then_some("true"),
            "aria-current": selected.then_some("page"),
            "data-selected": selected.then_some("true"),
            "data-highlighted": highlighted.then_some("true"),
            title,
            onfocus: move |event| {
                if let Some(handler) = onfocus {
                    handler.call(event);
                }
            },
            onmouseenter: move |event| {
                if let Some(handler) = onmouseenter {
                    handler.call(event);
                }
            },
            onclick: move |event| onclick.call(event),
            {children}
        }
    }
}

/// `df-menu-item`, its danger modifier, then any caller class.
pub fn menu_item_class(danger: bool, extra: Option<&str>) -> String {
    let mut class = String::from("df-menu-item");
    if danger {
        class.push_str(" df-menu-item--danger");
    }
    if let Some(extra) = extra.filter(|extra| !extra.is_empty()) {
        class.push(' ');
        class.push_str(extra);
    }
    class
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
