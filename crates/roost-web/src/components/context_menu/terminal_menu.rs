//! The right-click menu over a terminal pane. Touch capability is not a layout
//! mode: a touch-capable desktop still needs a cursor-anchored menu, so the
//! compact viewport is the only thing that swaps in a bottom action sheet.
//! Composed from the shared primitives in `context_menu.rs` — the surface, the
//! rows, the roving focus and the dismissal are all that module's.
//! Ports `apps/web/src/components/terminal/TerminalContextMenu.tsx` and
//! `TerminalSheetItem.tsx`.

use dioxus::prelude::*;

use crate::components::context_menu::{
    CtxMenuItem, CtxMenuSeparator, DEFAULT_MENU_Z_INDEX, ctx_menu_surface_style,
};

/// The menu's items, in v2's order, each with the test id the specs drive.
pub const TERMINAL_MENU_ITEMS: &[(&str, &str)] = &[
    ("ctx-open-link", "Open link"),
    ("ctx-copy-selection", "Copy"),
    ("ctx-paste", "Paste"),
    ("ctx-new-terminal", "New terminal"),
    ("ctx-attach", "Attach file"),
    ("ctx-spotlight", "Bring to front"),
    ("ctx-unspotlight", "Push back"),
    ("ctx-debug-start", "Start terminal debugging"),
    ("ctx-capture-diagnostics", "Capture terminal diagnostic"),
    ("ctx-debug-stop", "Stop terminal debugging"),
    ("ctx-close", "Close terminal"),
];

/// Which surface a menu is drawn on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TerminalMenuVariant {
    /// A cursor-anchored floating menu.
    #[default]
    Floating,
    /// A bottom sheet, for a compact viewport.
    Sheet,
}

impl TerminalMenuVariant {
    /// The `data-variant` spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Floating => "floating",
            Self::Sheet => "sheet",
        }
    }
}

/// Touch capability is not a layout mode, so ONLY the compact boundary decides:
/// a KDE session or a touch-capable desktop still right-clicks with a mouse.
pub fn uses_action_sheet(compact: bool) -> bool {
    compact
}

/// The cursor-anchored menu at viewport (`x`, `y`).
#[allow(clippy::too_many_arguments)]
#[component]
pub fn TerminalFloatingMenu(
    x: f64,
    y: f64,
    selection: String,
    show_spotlight: bool,
    show_push_back: bool,
    capture_phase: crate::components::terminal_chrome::capture_consent::CapturePhase,
    on_copy: EventHandler<String>,
    on_paste: EventHandler<()>,
    on_new_terminal: EventHandler<()>,
    on_attach: EventHandler<()>,
    on_spotlight: EventHandler<()>,
    on_push_back: EventHandler<()>,
    on_start_debugging: EventHandler<()>,
    on_capture_diagnostic: EventHandler<()>,
    on_stop_debugging: EventHandler<()>,
    on_close: EventHandler<()>,
) -> Element {
    let start_enabled = capture_phase.start_enabled();
    let stop_enabled = capture_phase.stop_enabled();
    rsx! {
        div {
            role: "menu",
            "aria-label": "Terminal pane actions",
            "data-testid": "terminal-context-menu",
            "data-variant": "floating",
            class: "df-menu-enter",
            style: ctx_menu_surface_style(x, y, DEFAULT_MENU_Z_INDEX),
            if !selection.is_empty() {
                CtxMenuItem {
                    testid: "ctx-copy-selection",
                    onclick: move |_event| on_copy.call(selection.clone()),
                    "Copy"
                }
                CtxMenuSeparator {}
            }
            CtxMenuItem {
                testid: "ctx-paste",
                onclick: move |_event| on_paste.call(()),
                "Paste"
            }
            CtxMenuSeparator {}
            CtxMenuItem {
                testid: "ctx-new-terminal",
                onclick: move |_event| on_new_terminal.call(()),
                "New terminal"
            }
            CtxMenuItem {
                testid: "ctx-attach",
                onclick: move |_event| on_attach.call(()),
                "Attach file"
            }
            if show_spotlight {
                CtxMenuSeparator {}
                CtxMenuItem {
                    testid: "ctx-spotlight",
                    onclick: move |_event| on_spotlight.call(()),
                    "Bring to front"
                }
            }
            if show_push_back {
                CtxMenuSeparator {}
                CtxMenuItem {
                    testid: "ctx-unspotlight",
                    onclick: move |_event| on_push_back.call(()),
                    "Push back"
                }
            }
            CtxMenuSeparator {}
            crate::components::terminal_chrome::capture_consent::CaptureStateRow {
                phase: capture_phase,
                detail: None,
            }
            CtxMenuItem {
                testid: "ctx-debug-start",
                disabled: !start_enabled,
                onclick: move |_event| on_start_debugging.call(()),
                "Start terminal debugging"
            }
            CtxMenuItem {
                testid: "ctx-capture-diagnostics",
                disabled: !start_enabled,
                onclick: move |_event| on_capture_diagnostic.call(()),
                "Capture terminal diagnostic"
            }
            CtxMenuItem {
                testid: "ctx-debug-stop",
                disabled: !stop_enabled,
                onclick: move |_event| on_stop_debugging.call(()),
                "Stop terminal debugging"
            }
            CtxMenuSeparator {}
            CtxMenuItem {
                testid: "ctx-close",
                danger: true,
                onclick: move |_event| on_close.call(()),
                "Close terminal"
            }
        }
    }
}

/// The compact-viewport sheet, which reaches the same actions by ordinary
/// directional travel rather than by roving focus: its rows are in the tab
/// order, because there is no cursor to move.
#[allow(clippy::too_many_arguments)]
#[component]
pub fn TerminalActionSheet(
    selection: String,
    on_copy: EventHandler<String>,
    on_paste: EventHandler<()>,
    on_new_terminal: EventHandler<()>,
    on_attach: EventHandler<()>,
    on_start_debugging: EventHandler<()>,
    on_capture_diagnostic: EventHandler<()>,
    on_stop_debugging: EventHandler<()>,
    on_close: EventHandler<()>,
    on_cancel: EventHandler<()>,
) -> Element {
    // The row is conditional, so the copy handler must not own the selection
    // string the condition reads.
    let has_selection = !selection.is_empty();
    let copy = {
        let selection = selection.clone();
        move |_event: MouseEvent| on_copy.call(selection.clone())
    };
    rsx! {
        div {
            "data-testid": "terminal-context-sheet-backdrop",
            "aria-hidden": "true",
            style: "position: fixed; inset: 0; background: var(--md-scrim); opacity: 0.5; z-index: 40;",
            onclick: move |_event: MouseEvent| on_cancel.call(()),
        }
        div {
            "data-testid": "terminal-context-menu",
            "data-variant": "sheet",
            style: SHEET_STYLE,
            SheetItem { testid: "ctx-paste", on_activate: move |_event: MouseEvent| on_paste.call(()), "Paste" }
            SheetItem { testid: "ctx-new-terminal", on_activate: move |_event: MouseEvent| on_new_terminal.call(()), "New terminal" }
            SheetItem { testid: "ctx-attach", on_activate: move |_event: MouseEvent| on_attach.call(()), "Attach file" }
            if has_selection {
                SheetItem { testid: "ctx-copy-selection", on_activate: copy, "Copy" }
            }
            SheetItem { testid: "ctx-debug-start", on_activate: move |_event: MouseEvent| on_start_debugging.call(()), "Start terminal debugging" }
            SheetItem { testid: "ctx-capture-diagnostics", on_activate: move |_event: MouseEvent| on_capture_diagnostic.call(()), "Capture terminal diagnostic" }
            SheetItem { testid: "ctx-debug-stop", on_activate: move |_event: MouseEvent| on_stop_debugging.call(()), "Stop terminal debugging" }
            SheetItem { testid: "ctx-close", danger: true, on_activate: move |_event: MouseEvent| on_close.call(()), "Close terminal" }
            SheetItem { testid: "ctx-cancel", on_activate: move |_event: MouseEvent| on_cancel.call(()), "Cancel" }
        }
    }
}

/// The sheet's own chrome: a bottom-anchored surface that rides above the soft
/// keyboard, because a keyboard that covers the sheet's last action is a sheet
/// the operator cannot finish.
const SHEET_STYLE: &str = "position: fixed; left: 0; right: 0; bottom: max(var(--kb-offset), 0px); z-index: 41; box-sizing: border-box; background: var(--bg-elev-2); border-top: var(--workbench-border-width) solid var(--border-strong); border-radius: var(--md-shape-md) var(--md-shape-md) 0 0; box-shadow: var(--md-elev-5); padding: var(--md-space-3) 0 calc(env(safe-area-inset-bottom, 0px) + var(--md-space-4)); max-height: calc(100dvh - max(var(--kb-offset), 0px) - var(--md-space-4)); overflow-y: auto; user-select: none; color: var(--text-hi);";

/// One sheet row: a full-width, focus-order-reachable action.
#[component]
fn SheetItem(
    testid: String,
    #[props(default)] danger: bool,
    on_activate: EventHandler<MouseEvent>,
    children: Element,
) -> Element {
    let class = crate::components::context_menu::menu_item_class(danger, None);
    rsx! {
        button {
            r#type: "button",
            class: class,
            "data-testid": testid,
            style: "display: flex; align-items: center; width: 100%; padding: var(--md-space-3) var(--md-space-4);",
            onclick: move |event: MouseEvent| on_activate.call(event),
            {children}
        }
    }
}
