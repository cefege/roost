//! `Dialog`: rust-ui's modal, a controlled compound (root, trigger, content, and
//! the parts in `dialog_parts.rs`). The caller owns `open`; the content mounts
//! only while open, so mounting it IS opening and dropping it IS closing. The
//! rust-ui setup script's `data-state` changes are made here in Rust. Focus moves
//! live in `dom.rs`, and their rules in `focus_scope.rs`. `sheet.rs` reuses the
//! panel, the root and the context from this file.

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;
use icons::common::IconType;
use tw_merge::tw_merge;

use super::dom::{
    FocusOpener, came_from_first_tabbable, focus_edge, focus_initial, focused_element,
    restore_focus,
};
use super::field::scoped_element_id;
use super::focus_scope::{AutoFocusRequest, sentinel_focus_edge};
use super::icon::Icon;

pub use super::dialog_parts::{
    DialogAction, DialogBody, DialogClose, DialogDescription, DialogFooter, DialogHeader,
    DialogTitle,
};

const TRIGGER_CLASS: &str = "inline-flex items-center justify-center gap-2 whitespace-nowrap rounded-md text-sm font-medium border bg-background shadow-xs hover:bg-accent hover:text-accent-foreground h-9 px-4 py-2 cursor-pointer";
const DIALOG_PANEL_CLASS: &str = "relative bg-background border rounded-2xl shadow-lg p-6 w-full max-w-[calc(100%-2rem)] max-h-[85vh] fixed top-[50%] left-[50%] translate-x-[-50%] translate-y-[-50%] z-100 transition-all duration-200 data-[state=closed]:opacity-0 data-[state=closed]:scale-95 data-[state=open]:opacity-100 data-[state=open]:scale-100";
const DIALOG_FULLSCREEN_PANEL_CLASS: &str = "relative bg-background shadow-lg p-6 w-full h-dvh max-w-none max-h-none fixed top-0 left-0 z-100 transition-all duration-200 data-[state=closed]:opacity-0 data-[state=closed]:scale-95 data-[state=open]:opacity-100 data-[state=open]:scale-100";

const CLOSE_BUTTON_CLASS: &str = "absolute top-4 right-4 p-1 rounded-sm focus:ring-2 focus:ring-offset-2 focus:outline-none [&_svg:not([class*='size-'])]:size-4 focus:ring-ring";
/// Whether the close button shows: as the caller says, or, when the caller does
/// not say, only when there is no footer to close the dialog with.
pub fn dialog_shows_close_button(show_close_button: Option<bool>, has_actions: bool) -> bool {
    show_close_button.unwrap_or(!has_actions)
}

/// The key that dismisses a dialog.
pub fn is_dismiss_key(key: &str) -> bool {
    key == "Escape"
}

/// The state the root shares with its parts. Signals, because the root
/// re-renders with new props while parts already mounted beneath it keep
/// reading the same state.
#[derive(Clone, PartialEq)]
pub(crate) struct DialogContext {
    pub(crate) content_id: String,
    pub(crate) title_id: String,
    pub(crate) description_id: String,
    pub(crate) open: Signal<bool>,
    pub(crate) on_open_change: Signal<EventHandler<bool>>,
    pub(crate) has_footer: Signal<bool>,
}

impl DialogContext {
    /// Ask the caller to change `open`. The dialog never closes itself.
    pub(crate) fn request_open(&self, open: bool) {
        let handler = *self.on_open_change.peek();
        handler.call(open);
    }
}

/// Which rust-ui modal a panel renders as. Dialog and Sheet share the open
/// state, focus trap and dismissal; their markup and attribute names differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModalKind {
    Dialog,
    Sheet,
    Drawer,
}

/// A controlled modal. The caller owns `open`; the root reports requests to
/// change it through `on_open_change`.
#[component]
pub fn Dialog(
    #[props(into, optional)] class: Option<String>,
    open: bool,
    on_open_change: EventHandler<bool>,
    children: Element,
) -> Element {
    rsx! {
        ModalRoot { data_name: "__Dialog", class, open, on_open_change, {children} }
    }
}

/// The shared root: a `w-fit` wrapper that provides the modal context.
#[component]
pub(crate) fn ModalRoot(
    data_name: &'static str,
    #[props(into, optional)] class: Option<String>,
    open: bool,
    on_open_change: EventHandler<bool>,
    children: Element,
) -> Element {
    let base_id = use_hook(|| scoped_element_id("dialog"));
    let context = use_context_provider(move || DialogContext {
        content_id: format!("{base_id}-content"),
        title_id: format!("{base_id}-title"),
        description_id: format!("{base_id}-description"),
        open: Signal::new(open),
        on_open_change: Signal::new(on_open_change),
        has_footer: Signal::new(false),
    });
    // The provider keeps the state it was first given; each render rewrites it
    // so the parts read the caller's current `open` and handler.
    let mut open_signal = context.open;
    if *open_signal.peek() != open {
        open_signal.set(open);
    }
    let mut handler_signal = context.on_open_change;
    handler_signal.set(on_open_change);
    let merged = tw_merge!("w-fit", class.as_deref().unwrap_or(""));
    rsx! {
        div { class: "{merged}", "data-name": data_name, {children} }
    }
}

/// The control that asks for the dialog to open.
#[component]
pub fn DialogTrigger(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let context = use_context::<DialogContext>();
    let trigger_id = format!("trigger_{}", context.content_id);
    let merged = tw_merge!(TRIGGER_CLASS, class.as_deref().unwrap_or(""));
    rsx! {
        button {
            class: "{merged}",
            id: "{trigger_id}",
            tabindex: "0",
            "data-dialog-trigger": "{context.content_id}",
            onclick: move |_| context.request_open(true),
            {children}
        }
    }
}

/// The open dialog. Renders nothing while closed.
#[component]
pub fn DialogContent(
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] test_id: Option<String>,
    #[props(into, optional)] style: Option<String>,
    #[props(into, optional)] scrim_test_id: Option<String>,
    show_close_button: Option<bool>,
    #[props(default = true)] close_on_backdrop_click: bool,
    on_open_auto_focus: Option<EventHandler<AutoFocusRequest>>,
    on_close_auto_focus: Option<EventHandler<AutoFocusRequest>>,
    #[props(default = false)] fullscreen: bool,
    children: Element,
) -> Element {
    let context = use_context::<DialogContext>();
    if !*context.open.read() {
        return rsx! {};
    }
    let panel_recipe = if fullscreen {
        DIALOG_FULLSCREEN_PANEL_CLASS
    } else {
        DIALOG_PANEL_CLASS
    };
    let panel_class = tw_merge!(panel_recipe, class.as_deref().unwrap_or(""));
    rsx! {
        ModalPanel {
            kind: ModalKind::Dialog,
            panel_class,
            test_id,
            style,
            scrim_test_id,
            show_close_button,
            close_on_backdrop_click,
            on_open_auto_focus,
            on_close_auto_focus,
            {children}
        }
    }
}

/// The mounted modal: backdrop, `role="dialog"` panel, focus trap and dismissal.
/// Mounting it opens the modal; dropping it closes it. The panel mounts with
/// `data-state="closed"` and flips to `open` once mounted, which is the state
/// change the rust-ui script made on click, so the open transition runs.
#[component]
pub(crate) fn ModalPanel(
    kind: ModalKind,
    panel_class: String,
    test_id: Option<String>,
    style: Option<String>,
    scrim_test_id: Option<String>,
    show_close_button: Option<bool>,
    close_on_backdrop_click: bool,
    on_open_auto_focus: Option<EventHandler<AutoFocusRequest>>,
    on_close_auto_focus: Option<EventHandler<AutoFocusRequest>>,
    #[props(default)] on_keydown: Option<EventHandler<KeyboardEvent>>,
    children: Element,
) -> Element {
    let context = use_context::<DialogContext>();
    let opener: Rc<RefCell<Option<FocusOpener>>> =
        use_hook(|| Rc::new(RefCell::new(focused_element())));
    let mut container = use_signal(|| None::<Rc<MountedData>>);
    let mut shown = use_signal(|| false);
    let has_footer = *context.has_footer.read();
    let shows_close = dialog_shows_close_button(show_close_button, has_footer);

    use_drop({
        let opener = opener.clone();
        move || {
            tracing::debug!(target: "ui", "dialog closed");
            let request = AutoFocusRequest::new();
            if let Some(handler) = on_close_auto_focus {
                handler.call(request.clone());
            }
            if !request.default_prevented()
                && let Some(opener) = opener.borrow().as_ref()
            {
                restore_focus(opener);
            }
        }
    });

    let on_sentinel_focus = move |event: FocusEvent| {
        if let Some(mounted) = container() {
            let edge = sentinel_focus_edge(came_from_first_tabbable(&event, &mounted));
            focus_edge(&mounted, edge);
        }
    };

    let is_shown = *shown.read();
    let state = if is_shown { "open" } else { "closed" };
    let pointer_events = if is_shown { "auto" } else { "none" };
    let backdrop_behavior = if close_on_backdrop_click {
        "auto"
    } else {
        "manual"
    };
    let panel_style = format!(
        "pointer-events: {pointer_events}; {}",
        style.unwrap_or_default()
    );
    let content_id = context.content_id.clone();
    let backdrop_id = format!("{content_id}_backdrop");
    let title_id = context.title_id.clone();
    let description_id = context.description_id.clone();
    let scrim_context = context.clone();
    let key_context = context.clone();
    let close_context = context.clone();
    let panel_backdrop = (kind == ModalKind::Dialog).then_some(backdrop_behavior);
    let backdrop_attribute =
        matches!(kind, ModalKind::Sheet | ModalKind::Drawer).then_some(backdrop_behavior);
    let panel_target = (kind == ModalKind::Dialog).then_some("target__dialog");
    let (backdrop_name, backdrop_class, panel_name, close_label, close_text) = match kind {
        ModalKind::Dialog => (
            "DialogBackdrop",
            "fixed inset-0 transition-opacity duration-200 pointer-events-none z-60 bg-black/50 data-[state=closed]:opacity-0 data-[state=open]:opacity-100",
            "DialogContent",
            "Close dialog",
            "Close Dialog",
        ),
        ModalKind::Sheet => (
            "SheetBackdrop",
            "fixed inset-0 z-60 bg-black/50 pointer-events-none opacity-0 transition-opacity duration-300 data-[state=open]:opacity-100",
            "SheetContent",
            "Close sheet",
            "Close Sheet",
        ),
        ModalKind::Drawer => (
            "DrawerBackdrop",
            "fixed inset-0 z-60 bg-black/50 pointer-events-none opacity-0 transition-opacity duration-300 data-[state=open]:opacity-100",
            "DrawerContent",
            "Close drawer",
            "Close Drawer",
        ),
    };
    let close_class = tw_merge!(CLOSE_BUTTON_CLASS, if shows_close { "" } else { "hidden" });

    rsx! {
        div {
            "data-name": backdrop_name,
            id: "{backdrop_id}",
            class: backdrop_class,
            "data-state": state,
            "data-testid": scrim_test_id,
            "data-backdrop": backdrop_attribute,
            style: "pointer-events: {pointer_events};",
            onclick: move |_| {
                if close_on_backdrop_click {
                    scrim_context.request_open(false);
                }
            },
        }
        div {
            id: "{content_id}",
            role: "dialog",
            tabindex: "-1",
            "aria-modal": "true",
            // Always named: an id with no element is ignored by assistive
            // technology, and the title and description render whenever the
            // caller places them, so the reference cannot be set conditionally.
            "aria-labelledby": "{title_id}",
            "aria-describedby": "{description_id}",
            "data-name": panel_name,
            class: "{panel_class}",
            "data-target": panel_target,
            "data-state": state,
            "data-backdrop": panel_backdrop,
            "data-testid": test_id,
            style: panel_style,
            onmounted: move |event: MountedEvent| {
                let mounted = event.data();
                container.set(Some(mounted.clone()));
                tracing::debug!(target: "ui", "dialog opened");
                let request = AutoFocusRequest::new();
                if let Some(handler) = on_open_auto_focus {
                    handler.call(request.clone());
                }
                if !request.default_prevented() {
                    focus_initial(&mounted);
                }
                shown.set(true);
            },
            onkeydown: move |event: KeyboardEvent| {
                if is_dismiss_key(&event.key().to_string()) {
                    event.prevent_default();
                    event.stop_propagation();
                    key_context.request_open(false);
                } else if let Some(on_keydown) = on_keydown {
                    on_keydown.call(event);
                }
            },
            span {
                class: "sr-only",
                "data-focus-trap": "",
                tabindex: "0",
                onfocus: on_sentinel_focus,
            }
            CloseButton {
                kind,
                class: close_class,
                content_id: content_id.clone(),
                label: close_label,
                text: close_text,
                context: close_context,
            }
            {children}
            span {
                class: "sr-only",
                "data-focus-trap": "",
                tabindex: "0",
                onfocus: on_sentinel_focus,
            }
        }
    }
}

/// The header close button. The attribute name is the rust-ui one for the
/// modal's kind, so each kind's script hook selects its own button.
#[component]
fn CloseButton(
    kind: ModalKind,
    class: String,
    content_id: String,
    label: &'static str,
    text: &'static str,
    context: DialogContext,
) -> Element {
    match kind {
        ModalKind::Dialog => rsx! {
            button {
                r#type: "button",
                class: "{class}",
                "data-dialog-close": "{content_id}",
                "aria-label": "{label}",
                onclick: move |_| context.request_open(false),
                span { class: "hidden", "{text}" }
                Icon { icon: IconType::X }
            }
        },
        ModalKind::Sheet => rsx! {
            button {
                r#type: "button",
                class: "{class}",
                "data-sheet-close": "{content_id}",
                "aria-label": "{label}",
                onclick: move |_| context.request_open(false),
                span { class: "hidden", "{text}" }
                Icon { icon: IconType::X }
            }
        },
        ModalKind::Drawer => rsx! {
            button {
                r#type: "button",
                class: "{class}",
                "data-drawer-close": "{content_id}",
                "aria-label": "{label}",
                onclick: move |_| context.request_open(false),
                span { class: "hidden", "{text}" }
                Icon { icon: IconType::X }
            }
        },
    }
}
