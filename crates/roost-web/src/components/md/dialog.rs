//! `Dialog`: the shared accessible modal. Ported from
//! `apps/web/src/components/Settings/md/Dialog.tsx` and the Kobalte dialog it
//! wrapped; `Sheet` composes it, and every modal surface (palette, rename,
//! browse, confirmation) renders inside one. Attaches `overlays.css`.
//!
//! What it owns, as Kobalte did for v2: the scrim, `role="dialog"` labelled by
//! its headline and described by its description, Escape and scrim dismissal, a
//! focus trap (a sentinel at each end), auto-focus into the content on open and
//! back to the opener on close — each auto-focus cancellable by the caller.
//! Closed, it renders nothing, so a spec asserting the dialog is gone sees zero
//! elements. The DOM moves live in `dom.rs`; their rules in `focus_scope.rs`.
//!
//! v2 portalled the dialog to `<body>` and marked everything outside it
//! `aria-hidden`. This renders in place (the overlay CSS is `position: fixed`)
//! and says the same thing to assistive technology with `aria-modal="true"`.

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;

use super::dom::{
    FocusOpener, came_from_first_tabbable, focus_edge, focused_element, restore_focus,
};
use super::focus_scope::{
    AutoFocusRequest, FocusEdge, VISUALLY_HIDDEN_STYLE, sentinel_focus_edge,
};
use super::form_field::scoped_element_id;
use super::icon::Icon;
use super::class_list::class_list;
use super::stylesheet::{OVERLAYS_STYLESHEET_HREF, use_md_stylesheet};

/// Whether the header's close button shows: as the caller says, or — when the
/// caller does not say — only when there is no action band to close it with.
pub fn dialog_shows_close_button(show_close_button: Option<bool>, has_actions: bool) -> bool {
    show_close_button.unwrap_or(!has_actions)
}

/// The key that dismisses a dialog.
pub fn is_dismiss_key(key: &str) -> bool {
    key == "Escape"
}

/// A modal dialog. `on_close` is the only way it closes: the caller owns `open`.
#[component]
pub fn Dialog(
    open: bool,
    on_close: EventHandler<()>,
    headline: Option<String>,
    children: Element,
    actions: Option<Element>,
    description: Option<Element>,
    class: Option<String>,
    test_id: Option<String>,
    on_open_auto_focus: Option<EventHandler<AutoFocusRequest>>,
    on_close_auto_focus: Option<EventHandler<AutoFocusRequest>>,
    show_close_button: Option<bool>,
) -> Element {
    use_md_stylesheet(OVERLAYS_STYLESHEET_HREF);
    if !open {
        return rsx! {};
    }
    rsx! {
        DialogContent {
            on_close,
            headline,
            actions,
            description,
            class,
            test_id,
            on_open_auto_focus,
            on_close_auto_focus,
            show_close_button,
            {children}
        }
    }
}

/// The open dialog. A separate component so that mounting it IS opening and
/// dropping it IS closing: the opener is captured on the first render and focus
/// is handed back when the component goes away, with no open-state bookkeeping.
#[component]
fn DialogContent(
    on_close: EventHandler<()>,
    headline: Option<String>,
    children: Element,
    actions: Option<Element>,
    description: Option<Element>,
    class: Option<String>,
    test_id: Option<String>,
    on_open_auto_focus: Option<EventHandler<AutoFocusRequest>>,
    on_close_auto_focus: Option<EventHandler<AutoFocusRequest>>,
    show_close_button: Option<bool>,
) -> Element {
    let base_id = use_hook(|| scoped_element_id("dialog"));
    let opener: Rc<RefCell<Option<FocusOpener>>> =
        use_hook(|| Rc::new(RefCell::new(focused_element())));
    let mut container = use_signal(|| None::<Rc<MountedData>>);

    use_drop({
        let opener = opener.clone();
        move || {
            tracing::debug!(target: "design", "dialog closed");
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

    let content_id = format!("{base_id}-content");
    let title_id = format!("{base_id}-title");
    let description_id = format!("{base_id}-description");
    let shows_close = dialog_shows_close_button(show_close_button, actions.is_some());
    let has_header = headline.is_some() || description.is_some() || shows_close;
    let labelled_by = headline.is_some().then(|| title_id.clone());
    let described_by = description.is_some().then(|| description_id.clone());

    let on_sentinel_focus = move |event: FocusEvent| {
        if let Some(mounted) = container() {
            let edge = sentinel_focus_edge(came_from_first_tabbable(&event, &mounted));
            focus_edge(&mounted, edge);
        }
    };

    rsx! {
        div {
            class: "roost-dialog__overlay",
            style: "pointer-events: auto;",
            "data-expanded": "",
            onpointerdown: move |event: PointerEvent| {
                event.prevent_default();
                on_close.call(());
            },
        }
        div {
            id: content_id,
            role: "dialog",
            tabindex: "-1",
            "aria-modal": "true",
            "aria-labelledby": labelled_by,
            "aria-describedby": described_by,
            "data-expanded": "",
            class: class_list(["roost-dialog", class.as_deref().unwrap_or("")]),
            "data-testid": test_id,
            onmounted: move |event: MountedEvent| {
                let mounted = event.data();
                container.set(Some(mounted.clone()));
                tracing::debug!(target: "design", "dialog opened");
                let request = AutoFocusRequest::new();
                if let Some(handler) = on_open_auto_focus {
                    handler.call(request.clone());
                }
                if !request.default_prevented() {
                    focus_edge(&mounted, FocusEdge::First);
                }
            },
            onkeydown: move |event: KeyboardEvent| {
                if is_dismiss_key(&event.key().to_string()) {
                    event.prevent_default();
                    event.stop_propagation();
                    on_close.call(());
                }
            },
            span {
                "data-focus-trap": "",
                tabindex: "0",
                style: VISUALLY_HIDDEN_STYLE,
                onfocus: on_sentinel_focus,
            }
            if has_header {
                div { class: "roost-dialog__header",
                    div { class: "roost-dialog__heading",
                        if let Some(headline) = headline {
                            h2 { id: title_id, class: "roost-dialog__title", {headline} }
                        }
                        if let Some(description) = description {
                            p { id: description_id, class: "roost-dialog__description", {description} }
                        }
                    }
                    if shows_close {
                        button {
                            r#type: "button",
                            class: "roost-dialog__close",
                            "aria-label": "Close",
                            onclick: move |_| on_close.call(()),
                            Icon { name: "close" }
                        }
                    }
                }
            }
            div { class: "roost-dialog__body", {children} }
            if let Some(actions) = actions {
                div { class: "roost-dialog__actions", {actions} }
            }
            span {
                "data-focus-trap": "",
                tabindex: "0",
                style: VISUALLY_HIDDEN_STYLE,
                onfocus: on_sentinel_focus,
            }
        }
    }
}
