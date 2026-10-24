//! Terminal pointer commands and the compact action drawer.
//! The terminal owns the target and command callbacks; Libero owns floating
//! placement, modal dismissal and the accessibility lifetime of each surface.

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;
use libero::{
    components::{Box, Button, Flex, Paper, Pictogram},
    hooks::{
        DrawerOptions, ModalScope, PopoverOptions, use_drawer, use_element, use_id, use_popover,
    },
    sx::sx,
    use_theme,
};

use super::focus_pointer_menu;
use super::return_focus::{focused_element, restore_focus};
use crate::components::layout::portal::Portal;
use crate::components::overlay_generation::OverlayGeneration;

pub const TERMINAL_MENU_ITEMS: &[(&str, &str)] = &[
    ("ctx-copy-selection", "Copy"),
    ("ctx-paste", "Paste"),
    ("ctx-find", "Find"),
    ("ctx-cancel", "Cancel"),
];

pub fn uses_action_sheet(compact: bool) -> bool {
    compact
}

#[must_use]
pub const fn should_open_terminal_context_menu(
    mouse_forwarded: bool,
    shift_held: bool,
    button: i16,
    trusted: bool,
) -> bool {
    !mouse_forwarded || shift_held || button != 2 || !trusted
}

#[component]
pub fn TerminalFloatingMenu(
    x: f64,
    y: f64,
    selection: String,
    on_copy: EventHandler<String>,
    on_find: EventHandler<()>,
    on_close: EventHandler<()>,
    #[props(default)] on_start_agent: Option<EventHandler<()>>,
) -> Element {
    let theme = use_theme();
    let opener = use_hook(|| Rc::new(focused_element()));
    use_drop(move || {
        if let Some(opener) = opener.as_ref() {
            restore_focus(opener);
        }
    });
    let mut open = use_signal(|| true);
    let mut focused = use_signal(|| false);
    let anchor = use_element();
    let menu_id = use_id().read().clone();
    let anchor_sx = use_hook(|| {
        sx().position("fixed")
            .width("0")
            .height("0")
            .pointer_events("none")
    });
    let panel_sx = use_hook(|| sx().min_width("13rem").padding("xs").z_index("1100"));
    let popover = use_popover(
        anchor,
        open(),
        PopoverOptions::new(theme.popover.gap, theme.popover.padding).dismiss(true),
    );
    popover.on_dismiss(move || {
        open.set(false);
        on_close.call(());
    });
    let focus_id = menu_id.clone();
    use_effect(move || {
        if popover.placed() && !*focused.peek() {
            focused.set(true);
            focus_pointer_menu(&focus_id);
        }
    });
    let keyboard_id = menu_id.clone();
    popover.show(open().then(|| rsx! {
        Paper {
            id: menu_id,
            role: "menu",
            aria_label: "Terminal actions",
            "data-testid": "terminal-context-menu",
            attributes: popover.floating_events(),
            style: popover.style(),
            onmounted: (*popover.floating()).mount(),
            onkeydown: move |event| run_terminal_menu_key(&event, &keyboard_id, move || {
                open.set(false);
                on_close.call(());
            }),
            sx: panel_sx,
            Flex { direction: "column", gap: "xs",
                if !selection.is_empty() {
                    Button { variant: "standard", role: "menuitem", tabindex: "-1",
                        "data-testid": "ctx-copy-selection",
                        onclick: move |_| on_copy.call(selection.clone()), "Copy"
                    }
                }
                if let Some(on_start_agent) = on_start_agent {
                    Button { variant: "standard", role: "menuitem", tabindex: "-1",
                        "data-testid": "ctx-start-agent", onclick: move |_| on_start_agent.call(()),
                        Pictogram { icon: pictogram_icons_lucide::bot::outlined, width: "16", height: "16" }
                        "Start agent here"
                    }
                }
                Button { variant: "standard", role: "menuitem", tabindex: "-1",
                    "data-testid": "ctx-find", onclick: move |_| on_find.call(()), "Find"
                }
            }
        }
    }));
    // The deck's transform changes the containing block of a fixed anchor.
    rsx! {
        Portal {
            Box { tabindex: "-1", aria_hidden: "true", onmounted: anchor.mount(),
                style: "left:{x}px;top:{y}px;", sx: anchor_sx,
            }
        }
    }
}

#[component]
pub fn TerminalActionSheet(
    menu_id: String,
    selection: String,
    on_copy: EventHandler<String>,
    on_paste: EventHandler<()>,
    on_find: EventHandler<()>,
    on_cancel: EventHandler<()>,
    #[props(default)] on_start_agent: Option<EventHandler<()>>,
) -> Element {
    let television = crate::components::layout::window_size::use_tv_layout();
    let (ordinary_sx, television_sx) = use_hook(|| {
        let ordinary = sx()
            .with("bottom", "max(var(--kb-offset), 0px)")
            .padding_bottom("calc(env(safe-area-inset-bottom, 0px) + 1rem)")
            .max_height("calc(100dvh - max(var(--kb-offset), 0px) - 1rem)");
        let television = ordinary
            .clone()
            .with("left", "var(--tv-overscan-inline)")
            .with("right", "var(--tv-overscan-inline)")
            .with(
                "bottom",
                "calc(max(var(--kb-offset), 0px) + var(--tv-overscan-block))",
            )
            .max_height(
                "calc(100dvh - max(var(--kb-offset), 0px) - var(--tv-overscan-block) - 1rem)",
            );
        (ordinary, television)
    });
    let fence = use_hook(|| Rc::new(RefCell::new(OverlayGeneration::default())));
    let drawer = use_drawer(
        DrawerOptions {
            anchor: "bottom".into(),
            size: "auto".into(),
            title: Some("Terminal actions".into()),
            close_button: true,
            sx: if television {
                television_sx
            } else {
                ordinary_sx
            }
            .into(),
            ..Default::default()
        },
        move |scope: ModalScope<()>| {
            let copy_text = selection.clone();
            rsx! {
                Flex { id: menu_id.clone(), direction: "column", gap: "xs",
                    "data-testid": "terminal-context-menu",
                    if !selection.is_empty() {
                        Button { variant: "standard", "data-testid": "ctx-copy-selection",
                            onclick: move |_| { on_copy.call(copy_text.clone()); scope.close(); }, "Copy"
                        }
                    }
                    Button { variant: "standard", "data-testid": "ctx-paste",
                        onclick: move |_| { on_paste.call(()); scope.close(); }, "Paste"
                    }
                    Button { variant: "standard", "data-testid": "ctx-find",
                        onclick: move |_| { on_find.call(()); scope.close(); }, "Find"
                    }
                    if let Some(on_start_agent) = on_start_agent {
                        Button { variant: "standard", "data-testid": "ctx-start-agent",
                            onclick: move |_| { on_start_agent.call(()); scope.close(); },
                            Pictogram { icon: pictogram_icons_lucide::bot::outlined, width: "16", height: "16" }
                            "Start agent here"
                        }
                    }
                    Button { variant: "standard", "data-testid": "ctx-cancel",
                        onclick: move |_| scope.close(), "Cancel"
                    }
                }
            }
        },
    );
    let opening_fence = fence.clone();
    use_effect(move || {
        let generation = opening_fence.borrow_mut().begin();
        let completion_fence = opening_fence.clone();
        drawer.open().onresult(move |_| {
            let current = completion_fence.borrow_mut().finish(generation);
            if current {
                on_cancel.call(());
            }
        });
    });
    use_drop(move || fence.borrow_mut().invalidate());
    rsx! {}
}

fn run_terminal_menu_key(event: &KeyboardEvent, menu_id: &str, on_close: impl FnOnce() + 'static) {
    #[cfg(target_arch = "wasm32")]
    if let Some(native) = event.data().downcast::<web_sys::KeyboardEvent>().cloned()
        && let Some(menu) = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.get_element_by_id(menu_id))
    {
        crate::components::context_menu::run_pointer_menu_key(&native, &menu, on_close);
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (event, menu_id, on_close);
}

#[cfg(test)]
mod tests {
    use super::should_open_terminal_context_menu;

    #[test]
    fn context_menu_respects_mouse_reporting_and_controller_synthetic_events() {
        assert!(!should_open_terminal_context_menu(true, false, 2, true));
        assert!(should_open_terminal_context_menu(true, true, 2, true));
        assert!(should_open_terminal_context_menu(true, false, 0, true));
        assert!(should_open_terminal_context_menu(true, false, 2, false));
        assert!(should_open_terminal_context_menu(false, false, 2, true));
    }
}
