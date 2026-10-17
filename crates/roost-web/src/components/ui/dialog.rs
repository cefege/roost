//! `Dialog`: the shared accessible modal, a controlled compound (root, trigger,
//! content, and the parts in `dialog_parts.rs`). The caller owns `open`; the
//! content mounts only while open, so mounting it IS opening and dropping it IS
//! closing, with the opener captured on that first render. Focus moves live in
//! `dom.rs`, and their rules in `focus_scope.rs`.

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;
use icons::IconType;
use tw_merge::tw_merge;

use super::button::{Button, ButtonVariant, IconButton};
use super::dom::{
    FocusOpener, came_from_first_tabbable, focus_edge, focus_initial, focused_element,
    restore_focus,
};
use super::field::scoped_element_id;
use super::focus_scope::{AutoFocusRequest, sentinel_focus_edge};

pub use super::dialog_parts::{
    DialogAction, DialogBody, DialogClose, DialogDescription, DialogFooter, DialogHeader,
    DialogTitle,
};

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
#[derive(Clone)]
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

/// A controlled modal. The caller owns `open`; the root reports requests to
/// change it through `on_open_change`.
#[component]
pub fn Dialog(open: bool, on_open_change: EventHandler<bool>, children: Element) -> Element {
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
    rsx! { {children} }
}

/// The control that asks for the dialog to open.
#[component]
pub fn DialogTrigger(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let context = use_context::<DialogContext>();
    rsx! {
        Button {
            variant: ButtonVariant::Outline,
            class,
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
    children: Element,
) -> Element {
    let context = use_context::<DialogContext>();
    if !*context.open.read() {
        return rsx! {};
    }
    rsx! {
        DialogPanel {
            class,
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

/// The mounted dialog: scrim, `role="dialog"` panel, focus trap and dismissal.
/// Mounting it opens the dialog; dropping it closes it.
#[component]
fn DialogPanel(
    class: Option<String>,
    test_id: Option<String>,
    style: Option<String>,
    scrim_test_id: Option<String>,
    show_close_button: Option<bool>,
    close_on_backdrop_click: bool,
    on_open_auto_focus: Option<EventHandler<AutoFocusRequest>>,
    on_close_auto_focus: Option<EventHandler<AutoFocusRequest>>,
    children: Element,
) -> Element {
    let context = use_context::<DialogContext>();
    let opener: Rc<RefCell<Option<FocusOpener>>> =
        use_hook(|| Rc::new(RefCell::new(focused_element())));
    let mut container = use_signal(|| None::<Rc<MountedData>>);
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

    let content_id = context.content_id.clone();
    let title_id = context.title_id.clone();
    let description_id = context.description_id.clone();
    let scrim_context = context.clone();
    let key_context = context.clone();
    let close_button_context = context.clone();

    rsx! {
        // The scrim takes pointer events so a click behind the dialog never
        // reaches the page, and only dismisses when the caller allows it.
        div {
            class: "fixed inset-0 z-1000 pointer-events-auto bg-black/55",
            "data-testid": scrim_test_id,
            onpointerdown: move |event: PointerEvent| {
                if close_on_backdrop_click {
                    event.prevent_default();
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
            class: tw_merge!(
                "bg-background border rounded-2xl shadow-lg p-6 w-full max-w-[calc(100%-2rem)] max-h-[85vh] fixed top-1/2 left-1/2 -translate-x-1/2 -translate-y-1/2 z-1000",
                class.as_deref().unwrap_or("")
            ),
            "data-testid": test_id,
            style: style,
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
            },
            onkeydown: move |event: KeyboardEvent| {
                if is_dismiss_key(&event.key().to_string()) {
                    event.prevent_default();
                    event.stop_propagation();
                    key_context.request_open(false);
                }
            },
            span {
                class: "sr-only",
                "data-focus-trap": "",
                tabindex: "0",
                onfocus: on_sentinel_focus,
            }
            if shows_close {
                IconButton {
                    icon: IconType::X,
                    label: "Close",
                    class: "absolute top-4 right-4 p-1 rounded-sm focus:ring-2 focus:ring-offset-2 focus:outline-none [&_svg:not([class*='size-'])]:size-4 focus:ring-ring",
                    onclick: move |_| close_button_context.request_open(false),
                }
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
