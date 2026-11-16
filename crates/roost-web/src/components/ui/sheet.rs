//! `Sheet`: a modal panel docked to a screen edge. It shares the root, the
//! context and the focus and dismissal behaviour in `dialog.rs`, and differs
//! only in its rust-ui markup: edge position, sheet attribute names, and the
//! sheet's own parts. `SheetSide` names the edge, as the rust-ui `direction`.

use dioxus::prelude::*;
use tw_merge::tw_merge;

use super::dialog::{DialogContext, ModalKind, ModalPanel, ModalRoot};
use super::focus_scope::AutoFocusRequest;

/// The screen edge a sheet slides in from. Centred sheets are not a sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SheetSide {
    Top,
    Bottom,
    Left,
    #[default]
    Right,
}

impl SheetSide {
    /// Position and size from rust-ui's `SheetDirection::class`. The
    /// `data-[state=*]` translate pair stands in for rust-ui's script, which
    /// toggled the translate classes from JavaScript.
    const fn class(self) -> &'static str {
        match self {
            Self::Top => {
                "top-0 left-0 w-full h-[400px] data-[state=closed]:-translate-y-full data-[state=open]:translate-y-0"
            }
            Self::Bottom => {
                "bottom-0 left-0 w-full h-[400px] data-[state=closed]:translate-y-full data-[state=open]:translate-y-0"
            }
            Self::Left => {
                "top-0 left-0 h-full w-[400px] data-[state=closed]:-translate-x-full data-[state=open]:translate-x-0"
            }
            Self::Right => {
                "top-0 right-0 h-full w-[400px] data-[state=closed]:translate-x-full data-[state=open]:translate-x-0"
            }
        }
    }
}

const SHEET_PANEL_BASE: &str = "fixed z-100 bg-card shadow-lg p-6 transition-transform duration-300 overflow-y-auto overscroll-y-contain";

/// The soft keyboard lift for a bottom sheet, merged after the caller's style so
/// it wins a conflict. Other edges take the caller's style unchanged.
fn bottom_sheet_style(side: SheetSide, caller_style: Option<String>) -> Option<String> {
    const KEYBOARD_LIFT: &str = "bottom: var(--sheet-kb-bottom); max-height: calc(90dvh - max(var(--kb-offset, 0px), 0px));";
    if side != SheetSide::Bottom {
        return caller_style;
    }
    match caller_style
        .as_deref()
        .map(str::trim)
        .filter(|style| !style.is_empty())
    {
        Some(style) => Some(format!("{}; {KEYBOARD_LIFT}", style.trim_end_matches(';'))),
        None => Some(KEYBOARD_LIFT.to_owned()),
    }
}

/// A controlled sheet. The caller owns `open`, as for `Dialog`.
#[component]
pub fn Sheet(open: bool, on_open_change: EventHandler<bool>, children: Element) -> Element {
    rsx! {
        ModalRoot { data_name: "Sheet", open, on_open_change, {children} }
    }
}

/// The open sheet. Renders nothing while closed.
#[component]
pub fn SheetContent(
    #[props(default)] side: SheetSide,
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] test_id: Option<String>,
    #[props(into, optional)] style: Option<String>,
    #[props(into, optional)] scrim_test_id: Option<String>,
    #[props(default = true)] show_close_button: bool,
    #[props(default = true)] close_on_backdrop_click: bool,
    on_open_auto_focus: Option<EventHandler<AutoFocusRequest>>,
    on_close_auto_focus: Option<EventHandler<AutoFocusRequest>>,
    #[props(default)] on_keydown: Option<EventHandler<KeyboardEvent>>,
    children: Element,
) -> Element {
    let context = use_context::<DialogContext>();
    if !*context.open.read() {
        return rsx! {};
    }
    let panel_class = tw_merge!(
        SHEET_PANEL_BASE,
        side.class(),
        class.as_deref().unwrap_or("")
    );
    rsx! {
        ModalPanel {
            kind: ModalKind::Sheet,
            panel_class,
            test_id,
            style: bottom_sheet_style(side, style),
            scrim_test_id,
            show_close_button: Some(show_close_button),
            close_on_backdrop_click,
            on_open_auto_focus,
            on_keydown,
            on_close_auto_focus,
            {children}
        }
    }
}

#[component]
pub fn SheetHeader(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged = tw_merge!("flex flex-col gap-0.5 p-4", class.as_deref().unwrap_or(""));
    rsx! { div { "data-name": "SheetHeader", class: "{merged}", {children} } }
}

#[component]
pub fn SheetTitle(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let context = use_context::<DialogContext>();
    let merged = tw_merge!("font-bold text-2xl", class.as_deref().unwrap_or(""));
    rsx! {
        h2 { "data-name": "SheetTitle", id: "{context.title_id}", class: "{merged}", {children} }
    }
}
