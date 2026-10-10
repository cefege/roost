//! The terminal context menu, with only actions backed by pane APIs.
//! Floating desktop menus use roving focus; compact layouts use the same
//! actions in a naturally focusable bottom sheet.

use dioxus::prelude::*;

use crate::components::context_menu::{
    CtxMenuItem, ctx_menu_surface_style, use_floating_menu_dismiss,
};
#[cfg(target_arch = "wasm32")]
use crate::components::context_menu::{MenuFocusEdge, focus_menu_edge};
use crate::components::md::{Icon, IconSize};
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

/// The cursor-anchored menu at viewport (`x`, `y`), moved on mount so it fits.
#[component]
pub fn TerminalFloatingMenu(
    x: f64,
    y: f64,
    menu_id: String,
    selection: String,
    on_copy: EventHandler<String>,
    on_paste: EventHandler<()>,
    on_find: EventHandler<()>,
    on_close: EventHandler<()>,
    #[props(default)] on_start_agent: Option<EventHandler<()>>,
) -> Element {
    use_floating_menu_dismiss(on_close, None, vec![menu_id.clone()]);
    #[cfg(target_arch = "wasm32")]
    {
        let id = menu_id.clone();
        use_hook(move || focus_menu_edge(&id, MenuFocusEdge::First));
    }
    let mut origin = use_signal(|| (x, y));
    let copy_selection = selection.clone();
    let key_menu_id = menu_id.clone();
    let (left, top) = origin();
    rsx! {
        div {
            id: menu_id,
            role: "menu",
            "aria-label": "Terminal actions",
            "data-testid": "terminal-context-menu",
            "data-variant": "floating",
            class: "df-menu-enter",
            style: ctx_menu_surface_style(
                left,
                top,
                crate::components::context_menu::DEFAULT_MENU_Z_INDEX,
            ),
            onmounted: move |event: MountedEvent| async move {
                let Ok(rect) = event.data().get_client_rect().await else {
                    return;
                };
                let viewport = (
                    crate::components::deck::deck_dom::viewport_width(),
                    crate::components::md::dom::viewport_height().unwrap_or(0.0),
                );
                origin.set(fit_menu_origin((x, y), (rect.width(), rect.height()), viewport));
            },
            onclick: move |event: MouseEvent| event.stop_propagation(),
            oncontextmenu: move |event: MouseEvent| event.prevent_default(),
            onkeydown: move |event: KeyboardEvent| run_terminal_menu_key(
                &event,
                &key_menu_id,
                move || on_close.call(()),
            ),
            if !selection.is_empty() {
                CtxMenuItem {
                    testid: "ctx-copy-selection",
                    onclick: move |_event| on_copy.call(copy_selection.clone()),
                    "Copy"
                }
            }
            if let Some(on_start_agent) = on_start_agent {
                CtxMenuItem {
                    testid: "ctx-start-agent",
                    onclick: move |_| on_start_agent.call(()),
                    Icon { name: "smart_toy", size: IconSize::Sm }
                    "Start agent here"
                }
            }
            CtxMenuItem { testid: "ctx-find", onclick: move |_| on_find.call(()), "Find" }
        }
    }
}

/// The compact-viewport sheet, whose actions are in ordinary tab order.
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
    let sheet_style = if crate::components::layout::window_size::use_tv_layout() {
        TV_SHEET_STYLE
    } else {
        SHEET_STYLE
    };
    #[cfg(target_arch = "wasm32")]
    {
        let id = menu_id.clone();
        use_hook(move || focus_menu_edge(&id, MenuFocusEdge::First));
    }
    rsx! {
        div {
            "data-testid": "terminal-context-sheet-backdrop",
            "aria-hidden": "true",
            style: "position: fixed; inset: 0; background: var(--md-scrim); opacity: 0.5; z-index: 40;",
            onclick: move |_event: MouseEvent| on_cancel.call(()),
        }
        div {
            id: menu_id,
            role: "menu",
            "aria-label": "Terminal actions",
            "data-testid": "terminal-context-menu",
            "data-variant": "sheet",
            style: sheet_style,
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
                    Icon { name: "smart_toy", size: IconSize::Sm }
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

const SHEET_STYLE: &str = "position: fixed; left: 0; right: 0; bottom: max(var(--kb-offset), 0px); z-index: 41; box-sizing: border-box; background: var(--bg-elev-2); border-top: var(--workbench-border-width) solid var(--border-strong); border-radius: var(--md-shape-md) var(--md-shape-md) 0 0; box-shadow: var(--md-elev-5); padding: var(--md-space-3) 0 calc(env(safe-area-inset-bottom, 0px) + var(--md-space-4)); max-height: calc(100dvh - max(var(--kb-offset), 0px) - var(--md-space-4)); overflow-y: auto; user-select: none; color: var(--text-hi);";
const TV_SHEET_STYLE: &str = "position: fixed; left: var(--tv-overscan-inline); right: var(--tv-overscan-inline); bottom: calc(max(var(--kb-offset), 0px) + var(--tv-overscan-block)); z-index: 41; box-sizing: border-box; background: var(--bg-elev-2); border-top: var(--workbench-border-width) solid var(--border-strong); border-radius: var(--md-shape-md) var(--md-shape-md) 0 0; box-shadow: var(--md-elev-5); padding: var(--md-space-3) 0 calc(env(safe-area-inset-bottom, 0px) + var(--md-space-4)); max-height: calc(100dvh - max(var(--kb-offset), 0px) - var(--tv-overscan-block) - var(--md-space-4)); overflow-y: auto; user-select: none; color: var(--text-hi);";

#[component]
fn SheetItem(testid: String, on_activate: EventHandler<MouseEvent>, children: Element) -> Element {
    let class = crate::components::context_menu::menu_item_class(false, None);
    rsx! {
        button {
            r#type: "button",
            role: "menuitem",
            class,
            "data-testid": testid,
            style: "display: flex; align-items: center; width: 100%; padding: var(--md-space-3) var(--md-space-4);",
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
