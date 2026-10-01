//! The chrome both sidebar row menus share: the click-away scrim, the fixed
//! menu surface at the pointer, roving keyboard focus with the first item
//! focused on open, and Escape handing focus back. The Portal + scrim half of
//! `apps/web/src/components/sidebar/SessionRowContextMenu.tsx` and
//! `FolderRowContextMenu.tsx`; the surface and keys are `context_menu`'s.

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;

use crate::components::context_menu::ctx_menu_surface_style;
use crate::components::md;

/// Above the scrim (99) that dismisses it.
const MENU_Z_INDEX: u32 = 100;
const SCRIM_STYLE: &str = "position: fixed; inset: 0; z-index: 99;";

/// A row menu at viewport (`x`, `y`) holding `children` items.
#[component]
pub fn ContextMenuFrame(
    x: f64,
    y: f64,
    menu_id: String,
    label: String,
    test_id: String,
    on_close: EventHandler<()>,
    children: Element,
) -> Element {
    // The row that opened this menu had focus, and a menu that takes it and
    // never gives it back strands the operator on `body`: the arrow keys stop
    // moving and Escape has nothing to hand focus to. Captured BEFORE the first
    // edge is focused, because that focus move is what would otherwise become
    // the remembered opener.
    let opener: Rc<RefCell<Option<md::dom::FocusOpener>>> =
        use_hook(|| Rc::new(RefCell::new(md::dom::focused_element())));
    use_drop(move || {
        if let Some(opener) = opener.borrow().as_ref() {
            md::dom::restore_focus(opener);
        }
    });
    #[cfg(target_arch = "wasm32")]
    {
        let menu_id = menu_id.clone();
        use_hook(move || {
            crate::components::context_menu::focus_menu_edge(
                &menu_id,
                crate::components::context_menu::MenuFocusEdge::First,
            );
        });
    }
    let key_menu_id = menu_id.clone();
    rsx! {
        div {
            style: SCRIM_STYLE,
            onclick: move |event: MouseEvent| {
                event.stop_propagation();
                on_close.call(());
            },
            oncontextmenu: move |event: MouseEvent| {
                event.prevent_default();
                on_close.call(());
            },
        }
        div {
            id: menu_id,
            role: "menu",
            "aria-label": label,
            "data-testid": test_id,
            class: "df-menu-enter",
            style: ctx_menu_surface_style(x, y, MENU_Z_INDEX),
            onclick: move |event: MouseEvent| event.stop_propagation(),
            onkeydown: move |event: KeyboardEvent| {
                #[cfg(target_arch = "wasm32")]
                super::dom::run_menu_key_by_id(&event, &key_menu_id, move || on_close.call(()));
                #[cfg(not(target_arch = "wasm32"))]
                let _ = (&event, &key_menu_id);
            },
            {children}
        }
    }
}
