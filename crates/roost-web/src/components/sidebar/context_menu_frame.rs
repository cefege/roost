//! The chrome both sidebar row menus share: a controlled ui `ContextMenu` opened
//! at the pointer, a click-away scrim beneath it, and focus returned to the row
//! on close. The `role="menu"` surface, roving keys and item rows come from
//! `ui`; the row supplies its items as children.

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;

use crate::components::context_menu::MenuFocusEdge;
use crate::components::ui::context_menu::{ContextMenu, ContextMenuContent};
use crate::components::ui::dom::{FocusOpener, focused_element, restore_focus};
use crate::components::ui::menu_overlay::{MenuContext, MenuPlacement, request_open};

/// A row menu at viewport (`x`, `y`) holding `children` items. `on_close` runs
/// on every dismissal: an item pick, Escape, an outside click or the scrim.
#[component]
pub fn ContextMenuFrame(
    x: f64,
    y: f64,
    test_id: String,
    label: &'static str,
    on_close: EventHandler<()>,
    children: Element,
) -> Element {
    // The row that opened this menu had focus, and a menu that takes it and
    // never gives it back strands the operator on `body`: the arrow keys stop
    // moving and Escape has nothing to hand focus to. Captured BEFORE the first
    // edge is focused, because that focus move is what would otherwise become
    // the remembered opener.
    let opener: Rc<RefCell<Option<FocusOpener>>> =
        use_hook(|| Rc::new(RefCell::new(focused_element())));
    use_drop(move || {
        if let Some(opener) = opener.borrow().as_ref() {
            restore_focus(opener);
        }
    });
    let on_open_change = EventHandler::new(move |open: bool| {
        if !open {
            on_close.call(());
        }
    });
    rsx! {
        ContextMenu { open: true, on_open_change, test_id: Some(test_id), aria_label: Some(label.to_owned()),
            OpenAtPointer { key: "{x},{y}", x, y }
            div {
                class: "fixed inset-0 z-[99]",
                onclick: move |event: MouseEvent| {
                    event.stop_propagation();
                    on_close.call(());
                },
                oncontextmenu: move |event: MouseEvent| {
                    event.prevent_default();
                    on_close.call(());
                },
            }
            ContextMenuContent { class: "z-[100] w-auto min-w-44", {children} }
        }
    }
}

/// Places the already-open menu at the pointer and focuses its first item. The
/// placement is written before the panel mounts, so the first render is at the
/// pointer and not at the origin.
#[component]
fn OpenAtPointer(x: f64, y: f64) -> Element {
    let context = try_use_context::<MenuContext>();
    use_hook(move || {
        if let Some(context) = context {
            request_open(
                context,
                Some(MenuPlacement::Point { x, y }),
                Some(MenuFocusEdge::First),
            );
        }
    });
    rsx! {}
}
