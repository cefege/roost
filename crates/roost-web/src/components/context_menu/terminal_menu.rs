//! The terminal context menu, with only actions backed by pane APIs. A desktop
//! right-click opens a ui `ContextMenu` at the pointer; compact layouts show the
//! same actions in a bottom `Sheet`, a modal docked above the on-screen keyboard
//! that adds a scrim, focus trap and Escape-to-close.

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;

use crate::components::context_menu::MenuFocusEdge;
#[cfg(target_arch = "wasm32")]
use crate::components::context_menu::focus_menu_edge;
use crate::components::deck::deck_dom;
use crate::components::ui::context_menu::{ContextMenu, ContextMenuContent, ContextMenuItem};
use crate::components::ui::dom::{FocusOpener, focused_element, restore_focus, viewport_height};
use crate::components::ui::menu_overlay::{MenuContext, MenuPlacement, request_open};
use crate::components::ui::{
    AutoFocusRequest, Button, ButtonVariant, Icon, IconType, Sheet, SheetContent, SheetSide,
    SheetTitle,
};

/// The actions this menu exposes, with stable test identifiers.
pub const TERMINAL_MENU_ITEMS: &[(&str, &str)] = &[
    ("ctx-copy-selection", "Copy"),
    ("ctx-paste", "Paste"),
    ("ctx-find", "Find"),
    ("ctx-cancel", "Cancel"),
];

/// Touch capability is not a layout mode, so ONLY the compact boundary decides:
/// a KDE session or a touch-capable desktop still right-clicks with a mouse.
pub fn uses_action_sheet(compact: bool) -> bool {
    compact
}

/// Whether a browser contextmenu gesture belongs to the pane menu.
///
/// Controller Y dispatches an untrusted right-button event; physical
/// right-click is trusted. Shift bypasses terminal mouse reporting.
#[must_use]
pub const fn should_open_terminal_context_menu(
    mouse_forwarded: bool,
    shift_held: bool,
    button: i16,
    trusted: bool,
) -> bool {
    !mouse_forwarded || shift_held || button != 2 || !trusted
}

/// The smallest gap kept between the menu and a viewport edge.
const VIEWPORT_EDGE_GAP_PX: f64 = 6.0;

/// Where a `width` × `height` menu opened at viewport (`x`, `y`) sits so it
/// stays on screen: flipped to the cursor's left or above it when it would
/// cross the right or bottom edge, and never past the top-left gap. An unknown
/// viewport (zero) leaves the cursor point as it is.
#[must_use]
pub fn fit_menu_origin(
    (x, y): (f64, f64),
    (width, height): (f64, f64),
    (viewport_width, viewport_height): (f64, f64),
) -> (f64, f64) {
    let fit = |at: f64, size: f64, viewport: f64| {
        if viewport <= 0.0 || at + size + VIEWPORT_EDGE_GAP_PX <= viewport {
            return at;
        }
        (at - size).max(VIEWPORT_EDGE_GAP_PX)
    };
    (
        fit(x, width, viewport_width),
        fit(y, height, viewport_height),
    )
}

/// The cursor-anchored menu at viewport (`x`, `y`). Focus returns to the pane
/// that had it on close, so the terminal keeps receiving keys after an action.
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
    // A menu that takes focus and never gives it back strands keyboard input on
    // `body`. The opener is captured before the menu moves focus to its first item.
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
    let copy_selection = selection.clone();
    let has_selection = !selection.is_empty();
    rsx! {
        ContextMenu { open: true, on_open_change, test_id: "terminal-context-menu", aria_label: "Terminal actions",
            FloatingMenuPlacement { key: "{x},{y}", x, y }
            ContextMenuContent { class: "w-auto min-w-52 flex select-none flex-col",
                if has_selection {
                    ContextMenuItem {
                        test_id: "ctx-copy-selection",
                        onclick: move |_: MouseEvent| on_copy.call(copy_selection.clone()),
                        "Copy"
                    }
                }
                if let Some(on_start_agent) = on_start_agent {
                    ContextMenuItem {
                        test_id: "ctx-start-agent",
                        onclick: move |_: MouseEvent| on_start_agent.call(()),
                        Icon { icon: IconType::Bot, class: "size-4" }
                        "Start agent here"
                    }
                }
                ContextMenuItem {
                    test_id: "ctx-find",
                    onclick: move |_: MouseEvent| on_find.call(()),
                    "Find"
                }
            }
        }
    }
}

/// Places the open floating menu at the pointer and focuses its first item.
/// The point is written during render, so the first paint is already at the
/// pointer. The flip past a viewport edge needs the rendered size, so it runs
/// once the panel has mounted.
#[component]
fn FloatingMenuPlacement(x: f64, y: f64) -> Element {
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
    use_effect(move || {
        let Some(context) = context else {
            return;
        };
        let menu_id = context.root.peek().menu_id.clone();
        let viewport = (deck_dom::viewport_width(), viewport_height().unwrap_or(0.0));
        let fitted = fit_menu_origin((x, y), rendered_menu_size(&menu_id), viewport);
        if fitted != (x, y) {
            request_open(
                context,
                Some(MenuPlacement::Point {
                    x: fitted.0,
                    y: fitted.1,
                }),
                None,
            );
        }
    });
    rsx! {}
}

/// The laid-out size of the mounted menu node, so the flip uses the width the
/// browser gave the surface. Zero off-wasm, which keeps the cursor point.
fn rendered_menu_size(menu_id: &str) -> (f64, f64) {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.get_element_by_id(menu_id))
            .map(|element| {
                let rect = element.get_bounding_client_rect();
                (rect.width(), rect.height())
            })
            .unwrap_or((0.0, 0.0))
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = menu_id;
        (0.0, 0.0)
    }
}

/// The compact-viewport action sheet: a bottom `Sheet`, so it has the dialog's
/// scrim, focus trap and Escape-to-close. A backdrop click, Escape, or Cancel
/// reports `on_cancel`; an action runs its handler, and the caller unmounts it.
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
    let has_selection = !selection.is_empty();
    let copy_selection = selection.clone();
    let key_menu_id = menu_id.clone();
    let focus_menu_id = menu_id.clone();
    let sheet_style = if crate::components::layout::window_size::use_tv_layout() {
        TV_SHEET_STYLE
    } else {
        SHEET_STYLE
    };
    let on_open_change = EventHandler::new(move |open: bool| {
        if !open {
            on_cancel.call(());
        }
    });
    let on_open_auto_focus = EventHandler::new(move |request: AutoFocusRequest| {
        // The dialog would focus its sentinel first; the menu edge takes its place.
        request.prevent_default();
        #[cfg(target_arch = "wasm32")]
        let _ = focus_menu_edge(&focus_menu_id, MenuFocusEdge::First);
        #[cfg(not(target_arch = "wasm32"))]
        let _ = &focus_menu_id;
    });
    rsx! {
        Sheet {
            open: true,
            on_open_change,
            SheetContent {
                side: SheetSide::Bottom,
                class: SHEET_CLASS,
                test_id: "terminal-context-menu",
                scrim_test_id: "terminal-context-sheet-backdrop",
                style: sheet_style,
                show_close_button: false,
                on_open_auto_focus: Some(on_open_auto_focus),
                SheetTitle { class: "sr-only", "Terminal actions" }
                div {
                    id: menu_id,
                    role: "menu",
                    "aria-label": "Terminal actions",
                    class: "flex flex-col",
                    onkeydown: move |event: KeyboardEvent| run_terminal_menu_key(
                        &event,
                        &key_menu_id,
                        move || on_cancel.call(()),
                    ),
                    if has_selection {
                        SheetItem {
                            testid: "ctx-copy-selection",
                            on_activate: move |_| on_copy.call(copy_selection.clone()),
                            "Copy"
                        }
                    }
                    SheetItem { testid: "ctx-paste", on_activate: move |_| on_paste.call(()), "Paste" }
                    SheetItem { testid: "ctx-find", on_activate: move |_| on_find.call(()), "Find" }
                    if let Some(on_start_agent) = on_start_agent {
                        SheetItem {
                            testid: "ctx-start-agent",
                            on_activate: move |_| on_start_agent.call(()),
                            Icon { icon: IconType::Bot, class: "size-4" }
                            "Start agent here"
                        }
                    }
                    SheetItem {
                        testid: "ctx-cancel",
                        on_activate: move |_| on_cancel.call(()),
                        "Cancel"
                    }
                }
            }
        }
    }
}

fn run_terminal_menu_key(event: &KeyboardEvent, menu_id: &str, on_escape: impl FnOnce() + 'static) {
    #[cfg(target_arch = "wasm32")]
    if let Some(native) = event.data().downcast::<web_sys::KeyboardEvent>().cloned()
        && let Some(menu) = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.get_element_by_id(menu_id))
    {
        crate::components::context_menu::run_menu_key(&native, &menu, on_escape, || {});
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (event, menu_id, on_escape);
}

// Only the placement lives inline: the bottom offset tracks the on-screen
// keyboard and the TV overscan, which change at runtime. Every property is set
// in both strings, since a Dioxus style string keeps any key it omits. The
// stacking layer comes from the dialog, so the scrim and panel share one.
const SHEET_CLASS: &str =
    "h-auto p-0 pt-3 rounded-t-lg border-t border-border text-foreground select-none";
const SHEET_STYLE: &str = "left: 0; right: 0; bottom: max(var(--kb-offset), 0px); padding-bottom: calc(env(safe-area-inset-bottom, 0px) + 1rem); max-height: calc(100dvh - max(var(--kb-offset), 0px) - 1rem);";
const TV_SHEET_STYLE: &str = "left: var(--tv-overscan-inline); right: var(--tv-overscan-inline); bottom: calc(max(var(--kb-offset), 0px) + var(--tv-overscan-block)); padding-bottom: calc(env(safe-area-inset-bottom, 0px) + 1rem); max-height: calc(100dvh - max(var(--kb-offset), 0px) - var(--tv-overscan-block) - 1rem);";

/// One action row: a `role="menuitem"` button, which `run_menu_key` moves between.
#[component]
fn SheetItem(testid: String, on_activate: EventHandler<MouseEvent>, children: Element) -> Element {
    rsx! {
        Button {
            variant: ButtonVariant::Ghost,
            class: Some(
                "w-full justify-start gap-3 rounded-none px-4 py-3 text-left text-base font-normal text-foreground focus:bg-accent"
                    .to_owned(),
            ),
            role: "menuitem",
            "data-testid": testid,
            onclick: move |event: MouseEvent| on_activate.call(event),
            {children}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{fit_menu_origin, should_open_terminal_context_menu};

    #[test]
    fn context_menu_respects_mouse_reporting_and_controller_synthetic_events() {
        assert!(!should_open_terminal_context_menu(true, false, 2, true));
        assert!(should_open_terminal_context_menu(true, true, 2, true));
        assert!(should_open_terminal_context_menu(true, false, 0, true));
        assert!(should_open_terminal_context_menu(true, false, 2, false));
        assert!(should_open_terminal_context_menu(false, false, 2, true));
    }

    #[test]
    fn a_menu_that_fits_opens_at_the_cursor() {
        assert_eq!(
            fit_menu_origin((100.0, 200.0), (180.0, 60.0), (1400.0, 900.0)),
            (100.0, 200.0)
        );
    }

    #[test]
    fn a_menu_past_the_right_or_bottom_edge_flips_to_the_cursors_other_side() {
        assert_eq!(
            fit_menu_origin((1300.0, 880.0), (180.0, 60.0), (1400.0, 900.0)),
            (1120.0, 820.0)
        );
    }

    #[test]
    fn a_menu_too_large_to_flip_stays_inside_the_top_left_gap() {
        assert_eq!(
            fit_menu_origin((50.0, 40.0), (180.0, 60.0), (200.0, 80.0)),
            (6.0, 6.0)
        );
    }

    #[test]
    fn an_unknown_viewport_leaves_the_cursor_point() {
        assert_eq!(
            fit_menu_origin((1300.0, 880.0), (180.0, 60.0), (0.0, 0.0)),
            (1300.0, 880.0)
        );
    }
}
