//! `Sheet`: a modal panel docked to a screen edge. It shares the root, the
//! context and the focus and dismissal behaviour in `dialog.rs`, and differs
//! only in its rust-ui markup: edge position, sheet attribute names, and the
//! sheet's own parts. `SheetSide` names the edge, as the rust-ui `direction`.

use dioxus::prelude::*;
use tw_merge::tw_merge;

use super::button::{ButtonSize, ButtonVariant};
use super::dialog::{DialogContext, ModalKind, ModalPanel, ModalRoot};
use super::dialog_parts::use_footer_presence;
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

const SHEET_TRIGGER_CLASS: &str = "inline-flex items-center justify-center gap-2 whitespace-nowrap rounded-md text-sm font-medium border bg-background shadow-xs hover:bg-accent hover:text-accent-foreground h-9 px-4 py-2 cursor-pointer";

const SHEET_CLOSE_BASE: &str = "inline-flex items-center justify-center gap-2 whitespace-nowrap rounded-md text-sm font-medium transition-all disabled:pointer-events-none disabled:opacity-50 [&_svg]:pointer-events-none [&_svg:not([class*='size-'])]:size-4 shrink-0 outline-none focus-visible:border-ring focus-visible:ring-ring/50 focus-visible:ring-[3px] hover:cursor-pointer";

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

/// The control that asks for the sheet to open.
#[component]
pub fn SheetTrigger(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let context = use_context::<DialogContext>();
    let merged = tw_merge!(SHEET_TRIGGER_CLASS, class.as_deref().unwrap_or(""));
    rsx! {
        button {
            "data-name": "SheetTrigger",
            "data-sheet-trigger": "{context.content_id}",
            class: "{merged}",
            onclick: move |_| context.request_open(true),
            {children}
        }
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
            on_close_auto_focus,
            {children}
        }
    }
}

/// A button that closes the sheet, in rust-ui's outline style.
#[component]
pub fn SheetClose(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let context = use_context::<DialogContext>();
    let variant = ButtonVariant::Outline.as_str();
    let size = ButtonSize::Default.as_str();
    let merged = tw_merge!(
        SHEET_CLOSE_BASE,
        variant,
        size,
        class.as_deref().unwrap_or("")
    );
    rsx! {
        button {
            "data-name": "SheetClose",
            "data-sheet-close": "{context.content_id}",
            class: "{merged}",
            onclick: move |_| context.request_open(false),
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

#[component]
pub fn SheetDescription(
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let context = use_context::<DialogContext>();
    let merged = tw_merge!("text-muted-foreground", class.as_deref().unwrap_or(""));
    rsx! {
        p { "data-name": "SheetDescription", id: "{context.description_id}", class: "{merged}", {children} }
    }
}

#[component]
pub fn SheetBody(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged = tw_merge!("flex flex-col gap-4", class.as_deref().unwrap_or(""));
    rsx! { div { "data-name": "SheetBody", class: "{merged}", {children} } }
}

/// The footer row. Presence turns the header close button off.
#[component]
pub fn SheetFooter(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    use_footer_presence();
    let merged = tw_merge!(
        "mt-auto flex flex-col gap-2 p-4",
        class.as_deref().unwrap_or("")
    );
    rsx! { footer { "data-name": "SheetFooter", class: "{merged}", {children} } }
}
