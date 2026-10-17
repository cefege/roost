//! The parts a `Dialog` is composed from: header, title, description, body,
//! footer, and the close and action buttons. Title and description read their
//! ids from the root's context, so the panel's `aria-labelledby` and
//! `aria-describedby` name the elements a caller actually rendered.

use dioxus::prelude::*;
use tw_merge::tw_merge;

use super::button::{Button, ButtonVariant};
use super::dialog::DialogContext;

/// The header block: title and description, stacked and centred on mobile.
#[component]
pub fn DialogHeader(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    rsx! {
        div {
            class: tw_merge!("flex flex-col gap-2 text-center sm:text-left", class.as_deref().unwrap_or("")),
            {children}
        }
    }
}

/// The dialog's accessible name.
#[component]
pub fn DialogTitle(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let context = use_context::<DialogContext>();
    rsx! {
        h2 {
            id: "{context.title_id}",
            class: tw_merge!("text-lg leading-none font-semibold", class.as_deref().unwrap_or("")),
            {children}
        }
    }
}

/// The dialog's accessible description.
#[component]
pub fn DialogDescription(
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let context = use_context::<DialogContext>();
    rsx! {
        p {
            id: "{context.description_id}",
            class: tw_merge!("text-muted-foreground text-sm", class.as_deref().unwrap_or("")),
            {children}
        }
    }
}

/// The scrollable-free body between header and footer.
#[component]
pub fn DialogBody(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    rsx! {
        div {
            class: tw_merge!("flex flex-col gap-4", class.as_deref().unwrap_or("")),
            {children}
        }
    }
}

/// The action row. Mounting it tells the root a footer exists, which hides the
/// header close button unless the caller asks for it.
#[component]
pub fn DialogFooter(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let context = use_context::<DialogContext>();
    let mut has_footer = context.has_footer;
    use_effect(move || {
        if let Ok(mut flag) = has_footer.try_write() {
            *flag = true;
        }
    });
    // A footer that unmounts with its dialog must not leave the header close
    // button hidden for the next open. `try_write` because the signal's owner
    // may already be gone when this drop runs.
    use_drop(move || {
        if let Ok(mut flag) = has_footer.try_write() {
            *flag = false;
        }
    });
    rsx! {
        footer {
            class: tw_merge!("flex flex-col-reverse gap-2 sm:flex-row sm:justify-end", class.as_deref().unwrap_or("")),
            {children}
        }
    }
}

/// An outline button that closes the dialog.
#[component]
pub fn DialogClose(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let context = use_context::<DialogContext>();
    rsx! {
        Button {
            variant: ButtonVariant::Outline,
            class,
            onclick: move |_| context.request_open(false),
            {children}
        }
    }
}

/// The primary button that closes the dialog.
#[component]
pub fn DialogAction(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let context = use_context::<DialogContext>();
    rsx! {
        Button {
            variant: ButtonVariant::Default,
            class,
            onclick: move |_| context.request_open(false),
            {children}
        }
    }
}
