//! `HoverCardPresentation`: rust-ui's hover-card surface for an owner that
//! already tracks hover and placement. `deck::pane_tab_hover_card` renders it;
//! `PaneStrip` owns dwell and dismissal.

use dioxus::prelude::*;
use tw_merge::tw_merge;

/// Registry surface for native hover owners that already have an anchor.
#[component]
pub fn HoverCardPresentation(
    #[props(into, optional)] class: Option<String>,
    #[props(optional)] onmouseenter: Option<EventHandler<MouseEvent>>,
    #[props(optional)] onmouseleave: Option<EventHandler<MouseEvent>>,
    #[props(optional)] onfocusin: Option<EventHandler<FocusEvent>>,
    #[props(optional)] onfocusout: Option<EventHandler<FocusEvent>>,
    #[props(extends = GlobalAttributes)] attributes: Vec<Attribute>,
    children: Element,
) -> Element {
    let card_class = tw_merge!(
        "overflow-visible relative z-50 p-4 rounded-lg border bg-card shadow-md w-64",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        div {
            "data-name": "HoverCardContent",
            class: "{card_class}",
            onmouseenter: move |event| {
                if let Some(handler) = onmouseenter { handler.call(event); }
            },
            onmouseleave: move |event| {
                if let Some(handler) = onmouseleave { handler.call(event); }
            },
            onfocusin: move |event| {
                if let Some(handler) = onfocusin { handler.call(event); }
            },
            onfocusout: move |event| {
                if let Some(handler) = onfocusout { handler.call(event); }
            },
            ..attributes,
            {children}
        }
    }
}
