//! `Drawer`: rust-ui's modal docked to a screen edge, with a drag handle and an
//! optional floating surface. It shares the root, context, focus and dismissal
//! with `dialog.rs` (`ModalKind::Drawer`) and renders rust-ui's `Drawer*` parts.
//! The caller owns `open`; the panel mounts only while open.

use dioxus::prelude::*;
use tw_merge::tw_merge;

use super::button::{ButtonSize, ButtonVariant};
use super::dialog::{DialogContext, ModalKind, ModalPanel, ModalRoot};
use super::dialog_parts::use_footer_presence;
use super::focus_scope::AutoFocusRequest;

/// The screen edge a drawer slides in from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DrawerPosition {
    #[default]
    Bottom,
    Top,
    Left,
    Right,
}

/// `Default` is attached to its edge; `Floating` is inset from every edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DrawerVariant {
    #[default]
    Default,
    Floating,
}

impl DrawerPosition {
    /// Position from rust-ui's `DrawerPosition` class recipe. The
    /// `data-[state=*]` translate pair stands in for rust-ui's script.
    const fn class(self) -> &'static str {
        match self {
            Self::Bottom => {
                "inset-x-0 bottom-0 rounded-t-[10px] border-t data-[state=closed]:translate-y-full data-[state=open]:translate-y-0"
            }
            Self::Top => {
                "inset-x-0 top-0 rounded-b-[10px] border-b data-[state=closed]:-translate-y-full data-[state=open]:translate-y-0"
            }
            Self::Left => {
                "inset-y-0 left-0 h-full w-[80vw] max-w-sm rounded-r-[10px] border-r data-[state=closed]:-translate-x-full data-[state=open]:translate-x-0"
            }
            Self::Right => {
                "inset-y-0 right-0 h-full w-[80vw] max-w-sm rounded-l-[10px] border-l data-[state=closed]:translate-x-full data-[state=open]:translate-x-0"
            }
        }
    }
}

const DRAWER_PANEL_BASE: &str = "fixed z-100 flex flex-col bg-background pt-3 pb-6 px-6 max-h-[90vh] overflow-auto shadow-lg transition-transform duration-300 ease-in-out";

const DRAWER_TRIGGER_CLASS: &str = "inline-flex items-center justify-center gap-2 whitespace-nowrap rounded-md text-sm font-medium border bg-background shadow-xs hover:bg-accent hover:text-accent-foreground h-9 px-4 py-2 cursor-pointer";

const DRAWER_CLOSE_BASE: &str = "inline-flex items-center justify-center gap-2 whitespace-nowrap rounded-md text-sm font-medium transition-all disabled:pointer-events-none disabled:opacity-50 [&_svg]:pointer-events-none [&_svg:not([class*='size-'])]:size-4 shrink-0 outline-none focus-visible:border-ring focus-visible:ring-ring/50 focus-visible:ring-[3px] hover:cursor-pointer";

/// A controlled drawer. The caller owns `open`, as for `Dialog`.
#[component]
pub fn Drawer(open: bool, on_open_change: EventHandler<bool>, children: Element) -> Element {
    rsx! {
        ModalRoot { data_name: "Drawer", open, on_open_change, {children} }
    }
}

/// The control that asks for the drawer to open.
#[component]
pub fn DrawerTrigger(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let context = use_context::<DialogContext>();
    let merged = tw_merge!(DRAWER_TRIGGER_CLASS, class.as_deref().unwrap_or(""));
    rsx! {
        button {
            "data-name": "DrawerTrigger",
            "data-drawer-trigger": "{context.content_id}",
            class: "{merged}",
            onclick: move |_| context.request_open(true),
            {children}
        }
    }
}

/// The open drawer. Renders nothing while closed. `dismissible` decides whether
/// a backdrop click closes it; the caller's `class` is appended last so it wins.
#[component]
pub fn DrawerContent(
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] test_id: Option<String>,
    #[props(into, optional)] style: Option<String>,
    #[props(default = true)] dismissible: bool,
    #[props(default)] position: DrawerPosition,
    #[props(default)] variant: DrawerVariant,
    on_open_auto_focus: Option<EventHandler<AutoFocusRequest>>,
    on_close_auto_focus: Option<EventHandler<AutoFocusRequest>>,
    children: Element,
) -> Element {
    let context = use_context::<DialogContext>();
    if !*context.open.read() {
        return rsx! {};
    }
    let floating_class = match variant {
        DrawerVariant::Default => "",
        DrawerVariant::Floating => "m-4 rounded-[10px] border",
    };
    let panel_class = tw_merge!(
        DRAWER_PANEL_BASE,
        position.class(),
        floating_class,
        class.as_deref().unwrap_or("")
    );
    rsx! {
        ModalPanel {
            kind: ModalKind::Drawer,
            panel_class,
            test_id,
            style,
            scrim_test_id: None,
            show_close_button: Some(false),
            close_on_backdrop_click: dismissible,
            on_open_auto_focus,
            on_close_auto_focus,
            {children}
        }
    }
}

/// The grab bar shown at the top of a drawer. Decorative: it has no behaviour.
#[component]
pub fn DrawerHandle() -> Element {
    rsx! {
        div { class: "block relative mx-auto mb-8 w-8 rounded-2xl opacity-70 hover:opacity-100 active:opacity-100 shrink-0 bg-[#e2e2e4] h-[5px]" }
    }
}

/// A button that closes the drawer, in rust-ui's outline style by default.
#[component]
pub fn DrawerClose(
    #[props(into, optional)] class: Option<String>,
    #[props(default = ButtonVariant::Outline)] variant: ButtonVariant,
    #[props(default = ButtonSize::Default)] size: ButtonSize,
    children: Element,
) -> Element {
    let context = use_context::<DialogContext>();
    let merged = tw_merge!(
        DRAWER_CLOSE_BASE,
        variant.as_str(),
        size.as_str(),
        class.as_deref().unwrap_or("")
    );
    rsx! {
        button {
            "data-name": "DrawerClose",
            "data-drawer-close": "{context.content_id}",
            class: "{merged}",
            onclick: move |_| context.request_open(false),
            {children}
        }
    }
}

#[component]
pub fn DrawerHeader(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged = tw_merge!("flex flex-col gap-2", class.as_deref().unwrap_or(""));
    rsx! { div { "data-name": "DrawerHeader", class: "{merged}", {children} } }
}

#[component]
pub fn DrawerTitle(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let context = use_context::<DialogContext>();
    let merged = tw_merge!(
        "text-lg leading-none font-semibold",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        h3 { "data-name": "DrawerTitle", id: "{context.title_id}", class: "{merged}", {children} }
    }
}

#[component]
pub fn DrawerDescription(
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let context = use_context::<DialogContext>();
    let merged = tw_merge!(
        "text-sm text-muted-foreground",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        p { "data-name": "DrawerDescription", id: "{context.description_id}", class: "{merged}", {children} }
    }
}

/// The drawer body, centred and capped at a readable measure.
#[component]
pub fn DrawerBody(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged = tw_merge!(
        "flex flex-col gap-4 mx-auto max-w-[500px]",
        class.as_deref().unwrap_or("")
    );
    rsx! { div { "data-name": "DrawerBody", class: "{merged}", {children} } }
}

#[component]
pub fn DrawerFooter(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    use_footer_presence();
    let merged = tw_merge!(
        "flex flex-col-reverse gap-2 sm:flex-row sm:justify-end",
        class.as_deref().unwrap_or("")
    );
    rsx! { footer { "data-name": "DrawerFooter", class: "{merged}", {children} } }
}
