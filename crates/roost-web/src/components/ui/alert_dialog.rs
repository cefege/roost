//! `AlertDialog`: a `Dialog` that a backdrop click cannot dismiss, for
//! confirmations where a stray click must not decide the outcome. The parts are
//! the dialog's own; only the dismissal policy differs, and only in
//! `AlertDialogContent`.

use dioxus::prelude::*;

use super::dialog::{
    Dialog, DialogAction, DialogClose, DialogContent, DialogDescription, DialogFooter,
    DialogHeader, DialogTitle, DialogTrigger,
};
use super::dialog_parts::DialogBody;
use super::focus_scope::AutoFocusRequest;

/// A controlled confirmation. The caller owns `open`, as for `Dialog`.
#[component]
pub fn AlertDialog(open: bool, on_open_change: EventHandler<bool>, children: Element) -> Element {
    rsx! { Dialog { open, on_open_change, {children} } }
}

#[component]
pub fn AlertDialogTrigger(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    rsx! { DialogTrigger { class, {children} } }
}

/// The confirmation panel. A backdrop click does nothing: the only ways out are
/// the actions, so a stray click cannot choose an outcome.
#[component]
pub fn AlertDialogContent(
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] test_id: Option<String>,
    show_close_button: Option<bool>,
    on_open_auto_focus: Option<EventHandler<AutoFocusRequest>>,
    on_close_auto_focus: Option<EventHandler<AutoFocusRequest>>,
    children: Element,
) -> Element {
    rsx! {
        DialogContent {
            class,
            test_id,
            show_close_button,
            close_on_backdrop_click: false,
            on_open_auto_focus,
            on_close_auto_focus,
            {children}
        }
    }
}

#[component]
pub fn AlertDialogHeader(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    rsx! { DialogHeader { class, {children} } }
}

#[component]
pub fn AlertDialogTitle(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    rsx! { DialogTitle { class, {children} } }
}

#[component]
pub fn AlertDialogDescription(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    rsx! { DialogDescription { class, {children} } }
}

#[component]
pub fn AlertDialogBody(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    rsx! { DialogBody { class, {children} } }
}

#[component]
pub fn AlertDialogFooter(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    rsx! { DialogFooter { class, {children} } }
}

/// The non-committing choice that closes the confirmation.
#[component]
pub fn AlertDialogCancel(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    rsx! { DialogClose { class, {children} } }
}

/// The committing choice that closes the confirmation. The caller performs the
/// action in its own handler, before or after `open` goes false.
#[component]
pub fn AlertDialogAction(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    rsx! { DialogAction { class, {children} } }
}
