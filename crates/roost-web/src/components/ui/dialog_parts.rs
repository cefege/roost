//! The non-panel parts of `Dialog`: header, title, description, body, footer,
//! and the close and action buttons. Each renders rust-ui's element type and
//! class string. The panel and root live in `dialog.rs`, which provides the
//! `DialogContext` these parts read.

use super::dialog::DialogContext;
use dioxus::prelude::*;
use tw_merge::tw_merge;

const CLOSE_OUTLINE_CLASS: &str = "inline-flex items-center justify-center gap-2 whitespace-nowrap rounded-md text-sm font-medium border bg-background shadow-xs hover:bg-accent hover:text-accent-foreground h-9 px-4 py-2 cursor-pointer";

const ACTION_CLASS: &str = "inline-flex items-center justify-center gap-2 whitespace-nowrap rounded-md text-sm font-medium bg-primary text-primary-foreground shadow-xs hover:bg-primary/90 h-9 px-4 py-2 cursor-pointer";

/// Marks the root as having a footer so the close button hides, and clears the
/// mark when the footer unmounts.
pub(crate) fn use_footer_presence() {
    let mut context = use_context::<DialogContext>();
    use_effect(move || {
        context.has_footer.set(true);
    });
    use_drop(move || {
        context.has_footer.set(false);
    });
}

/// The header block.
#[component]
pub fn DialogHeader(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged = tw_merge!(
        "flex flex-col gap-2 text-center sm:text-left",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        div { class: "{merged}", {children} }
    }
}

/// The dialog title, bound to the panel by `aria-labelledby`.
#[component]
pub fn DialogTitle(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let context = use_context::<DialogContext>();
    let merged = tw_merge!(
        "text-lg leading-none font-semibold",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        h3 { id: "{context.title_id}", class: "{merged}", {children} }
    }
}

/// The dialog description, bound to the panel by `aria-describedby`.
#[component]
pub fn DialogDescription(
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let context = use_context::<DialogContext>();
    let merged = tw_merge!(
        "text-muted-foreground text-sm",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        p { id: "{context.description_id}", class: "{merged}", {children} }
    }
}

/// The scrolling body between header and footer.
#[component]
pub fn DialogBody(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged = tw_merge!("flex flex-col gap-4", class.as_deref().unwrap_or(""));
    rsx! {
        div { class: "{merged}", {children} }
    }
}

/// The footer row. Presence turns the header close button off.
#[component]
pub fn DialogFooter(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    use_footer_presence();
    let merged = tw_merge!(
        "flex flex-col-reverse gap-2 sm:flex-row sm:justify-end",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        footer { class: "{merged}", {children} }
    }
}

/// A footer button that closes the dialog, in rust-ui's outline style.
#[component]
pub fn DialogClose(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let context = use_context::<DialogContext>();
    let merged = tw_merge!(CLOSE_OUTLINE_CLASS, class.as_deref().unwrap_or(""));
    rsx! {
        button {
            r#type: "button",
            class: "{merged}",
            "data-dialog-close": "{context.content_id}",
            "aria-label": "Close dialog",
            onclick: move |_| context.request_open(false),
            {children}
        }
    }
}

/// A primary footer button that closes the dialog.
#[component]
pub fn DialogAction(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let context = use_context::<DialogContext>();
    let merged = tw_merge!(ACTION_CLASS, class.as_deref().unwrap_or(""));
    rsx! {
        button {
            r#type: "button",
            class: "{merged}",
            "data-dialog-close": "{context.content_id}",
            "aria-label": "Close dialog",
            onclick: move |_| context.request_open(false),
            {children}
        }
    }
}
