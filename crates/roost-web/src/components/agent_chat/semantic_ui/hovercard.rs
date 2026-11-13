//! Native hover-card ownership and canonical dwell/dismiss behavior.
//! Prop trees have no controller IDs and reuse the surface's readonly renderers.
//! The open card borrows the active conversation's blob projection.

mod content;
mod dismiss;
mod timing;
pub(super) use timing::HoverCardScope;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use dioxus::prelude::*;
use serde_json::Value;

use crate::components::deck::deck_dom::Timeout;
use crate::components::ui::field::scoped_element_id;
use crate::components::ui::{
    HoverCardPresentation, anchored_panel::anchored_viewport_placement_style,
};

use super::media::MediaContext;
use crate::pump::use_store;
use dismiss::DismissEvents;
use timing::{CLOSE_DELAY_MS, now_ms};

/// A node's declared hover card over its own content.
///
/// The wrapper is a plain anchor (never a focusable trigger around interactive
/// children); dwell, grace and dismissal belong to this owner, and the card is
/// the stock `HoverCardPresentation` placed from the measured anchor.
#[component]
pub(super) fn NativeHoverCard(card: Value, surface: String, children: Element) -> Element {
    let anchor = use_hook(|| scoped_element_id("omp-hovercard"));
    let mut open = use_signal(|| false);
    let timer = use_hook(|| Rc::new(RefCell::new(None::<Timeout>)));
    let dismiss = use_hook(|| Rc::new(RefCell::new(None::<DismissEvents>)));
    let family = use_hook(|| try_consume_context::<HoverCardScope>().unwrap_or_default());
    let membership = use_hook(|| Rc::new(RefCell::new(family.membership())));
    use_effect(move || membership.borrow_mut().set_open(open(), now_ms()));
    let dismiss_timer = timer.clone();
    use_effect(move || {
        *dismiss.borrow_mut() = if open() {
            let timer = dismiss_timer.clone();
            DismissEvents::install(move || {
                *timer.borrow_mut() = None;
                open.set(false);
            })
        } else {
            None
        };
    });
    let hover_enter = {
        let timer = timer.clone();
        let family = family.clone();
        move |_| {
            *timer.borrow_mut() = Timeout::after(family.delay_ms(now_ms()), move || open.set(true));
        }
    };
    let hover_leave = {
        let timer = timer.clone();
        move |_| {
            *timer.borrow_mut() = Timeout::after(CLOSE_DELAY_MS, move || open.set(false));
        }
    };
    let card_enter = {
        let timer = timer.clone();
        move |_| {
            *timer.borrow_mut() = None;
        }
    };
    let card_leave = {
        let timer = timer.clone();
        move |_| {
            *timer.borrow_mut() = Timeout::after(CLOSE_DELAY_MS, move || open.set(false));
        }
    };
    let card_class = if card.get("k").and_then(Value::as_str) == Some("md") {
        "max-h-96 w-3xl overflow-y-auto pointer-events-none"
    } else {
        "max-h-96 w-96 overflow-y-auto"
    };
    let placement = if open() {
        anchored_viewport_placement_style(&anchor, false)
            .map(|style| format!("position: fixed; {style}"))
            .unwrap_or_else(|| "position: fixed;".to_owned())
    } else {
        String::new()
    };
    rsx! {
        div {
            id: "{anchor}",
            class: "min-w-0",
            title: open().then_some(""),
            onmouseenter: hover_enter,
            onmouseleave: hover_leave,
            onpointerdown: move |_| {
                *timer.borrow_mut() = None;
                open.set(false);
            },
            {children}
        }
        if open() {
            HoverCardPresentation {
                class: card_class,
                role: "tooltip",
                style: placement,
                onmouseenter: card_enter,
                onmouseleave: card_leave,
                NativeHoverCardBody { card: card.clone(), surface: surface.clone() }
            }
        }
    }
}

#[component]
fn NativeHoverCardBody(card: Value, surface: String) -> Element {
    let empty = BTreeMap::new();
    let Some(context) = try_use_context::<Signal<MediaContext>>() else {
        return content::render(&card, &surface, &empty);
    };
    let context = context.read();
    let pump = use_store();
    let core = pump.core();
    let core = core.borrow();
    let blobs = core
        .store()
        .agent_chat
        .transcripts
        .get(&context.conversation_id)
        .and_then(|loaded| loaded.transcript.omp_ui.as_ref())
        .map(|document| &document.blobs)
        .unwrap_or(&empty);
    content::render(&card, &surface, blobs)
}
