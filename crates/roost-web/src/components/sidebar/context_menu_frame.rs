//! Sidebar pointer menus preserve the row target and command callbacks.
//! Libero places and dismisses the Paper through a fixed coordinate anchor.
//! The captured opener, never the synthetic anchor, receives focus on close.

use crate::components::context_menu::focus_pointer_menu;
use crate::components::context_menu::return_focus::{focused_element, restore_focus};
use crate::components::layout::portal::Portal;
use dioxus::prelude::*;
use libero::{
    components::{Box, Paper},
    hooks::{PopoverOptions, use_element, use_popover},
    sx::sx,
    use_theme,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[component]
pub fn ContextMenuFrame(
    x: f64,
    y: f64,
    test_id: String,
    label: &'static str,
    on_close: EventHandler<()>,
    children: Element,
) -> Element {
    let theme = use_theme();
    let anchor = use_element();
    let opener = use_hook(|| Rc::new(focused_element()));
    let focused = use_hook(|| Rc::new(Cell::new(false)));
    let point = use_hook(|| Rc::new(RefCell::new(((x, y), 0_u64))));
    let remeasure = {
        let mut point = point.borrow_mut();
        if point.0 != (x, y) {
            point.0 = (x, y);
            point.1 = point.1.wrapping_add(1);
            focused.set(false);
        }
        point.1
    };
    let menu_sx = use_hook(|| sx().padding("xs").min_width("11rem").z_index("100"));
    let anchor_sx = use_hook(|| {
        sx().position("fixed")
            .width("0")
            .height("0")
            .pointer_events("none")
    });
    let popover = use_popover(
        anchor,
        true,
        PopoverOptions::new(theme.popover.gap, theme.popover.padding)
            .remeasure(remeasure)
            .dismiss(true),
    );
    popover.on_dismiss(move || {
        popover.show(None);
        on_close.call(());
    });
    let menu_id = test_id.clone();
    use_effect(move || {
        if popover.placed() && !focused.replace(true) {
            focus_pointer_menu(&menu_id);
        }
    });
    use_drop(move || {
        if let Some(opener) = opener.as_ref() {
            restore_focus(opener);
        }
    });
    let keyboard_id = test_id.clone();
    let menu_keydown = move |event: KeyboardEvent| {
        #[cfg(target_arch = "wasm32")]
        if let Some(native) = event.data().downcast::<web_sys::KeyboardEvent>()
            && let Some(menu) = web_sys::window()
                .and_then(|window| window.document())
                .and_then(|document| document.get_element_by_id(&keyboard_id))
        {
            crate::components::context_menu::run_pointer_menu_key(native, &menu, move || {
                on_close.call(())
            });
        }
        #[cfg(not(target_arch = "wasm32"))]
        let _ = (event, &keyboard_id, on_close);
    };
    popover.show(Some(rsx! {
        Paper { id: test_id.clone(), "data-testid": test_id,
            role: "menu", aria_label: label, tabindex: "-1",
            sx: menu_sx, style: popover.style(),
            onmounted: (*popover.floating()).mount(),
            onkeydown: menu_keydown, attributes: popover.floating_events(),
            {children}
        }
    }));
    rsx! {
        Portal {
            Box { aria_hidden: "true", tabindex: "-1", sx: anchor_sx,
                style: "left:{x}px;top:{y}px;", onmounted: anchor.mount(),
                attributes: popover.anchor_events(),
            }
        }
    }
}
