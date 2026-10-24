//! One toast surface inside the notification dock: the kind presentation, the
//! compact two-row action layout, the copy affordance for error and detail
//! text, and the hover/focus hold that freezes auto-dismiss and rings the toast's
//! target session. Rendered by `toast_stack`; identity, window and dismissal
//! are `roost_client_core::store::toasts`, read here rather than reimplemented.
//! Ports `apps/web/src/components/notifications/ToastCard.tsx`.

use dioxus::prelude::*;
use libero::{
    components::{Box, Flex, Indicator, Paper, Text},
    sx::sx,
};

mod actions;
use actions::ToastActionRow;

use roost_client_core::store::selectors::session_by_id;
use roost_client_core::store::toasts::{
    ToastId, ToastIntent, ToastKind, dismiss_toast, hold_toast_dismiss, release_toast_dismiss,
    take_toast_action,
};

use super::notify_target::NotifyTarget;
use super::store_write::write_store;
use crate::components::deck::deck_dom;
use crate::components::hooks::use_copy_clipboard::{copy_text_then, use_copy_clipboard};
use crate::components::layout::window_size::use_is_compact;
use crate::components::terminal::dom::now_ms;
use crate::pump::{Pump, use_store};
use crate::router_state::use_navigate;
use crate::terminal_href::terminal_href;

const fn toast_color(kind: ToastKind) -> &'static str {
    match kind {
        ToastKind::Ok => "success",
        ToastKind::Warn => "warning",
        ToastKind::Err => "error",
    }
}

/// The message, its window and its actions, read from the store on every render.
#[component]
pub fn ToastCard(toast_id: ToastId) -> Element {
    let pump = use_store();
    let navigate = use_navigate();
    let compact = use_is_compact();
    let notify_target = use_context::<NotifyTarget>();
    let (copy_to_clipboard, copied) = use_copy_clipboard(None);
    let mut copied_text = use_signal(|| None::<String>);
    let held = use_signal(|| false);
    let entrance_sx = use_hook(|| {
        sx().with("user-select", "text")
            .with("animation", "roost-toast-in 200ms cubic-bezier(0, 0, 0, 1)")
            .media(
                "(prefers-reduced-motion: reduce)",
                sx().with("animation", "none"),
            )
    });
    let card_sx = use_hook(|| {
        sx().position("relative")
            .display("flex")
            .flex_direction("column")
            .gap("xs")
            .overflow("hidden")
            .padding("sm")
            .white_space("pre-wrap")
            .with("overflow-wrap", "anywhere")
    });
    let headline_sx = use_hook(|| {
        sx().min_width("0")
            .flex("1")
            .overflow("hidden")
            .with("overscroll-behavior-y", "auto")
            .display("-webkit-box")
            .with("-webkit-line-clamp", "4")
            .with("-webkit-box-orient", "vertical")
            .with("user-select", "text")
    });
    let detail_sx = use_hook(|| {
        sx().margin("0")
            .max_height("9rem")
            .overflow("auto")
            .padding("xs")
            .border_radius("sm")
            .background("surface")
            .white_space("pre-wrap")
            .with("overflow-wrap", "anywhere")
            .with("font-family", "var(--term-font-family)")
    });
    let countdown_sx = use_hook(|| {
        sx().position("absolute")
            .with("left", "0")
            .with("right", "0")
            .with("bottom", "0")
            .height("var(--workbench-border-width)")
            .with("transform-origin", "left")
            .background("currentColor")
            .with("animation", "roost-countdown-drain linear forwards")
            .media(
                "(prefers-reduced-motion: reduce)",
                sx().with("animation", "none"),
            )
    });

    let toast = {
        let core = pump.core();
        let core = core.borrow();
        core.store().toasts.toast(&toast_id).cloned()
    };
    let Some(toast) = toast else {
        return rsx! {};
    };

    // A tap on a touch device synthesizes `mouseenter` with no `mouseleave`,
    // which would freeze the dismissal and pin the ring until the user pressed
    // ✕. Width alone does not catch a touch laptop, so this asks the pointer.
    let hoverable = !compact && !deck_dom::is_touch_device();
    let target = toast.target_session_id.clone();
    let hold_pump = pump.clone();
    let release_pump = pump.clone();
    let focus_pump = pump.clone();
    let focus_release_pump = pump.clone();

    // The hold is a pointer OR focus arrival, so the body is written once per
    // family of event. `Signal::set` takes `&mut self` and `NotifyTarget` is
    // `Copy`, so each closure takes its own handle to both.
    let hold: EventHandler<MouseEvent> = EventHandler::new({
        let id = toast_id.clone();
        let pointer_target = target.clone();
        move |_event| {
            let mut held = held;
            let mut ring = notify_target;
            hold_toast_dismissal(
                &hold_pump,
                &id,
                &mut held,
                &mut ring,
                hoverable,
                pointer_target.as_deref(),
            );
        }
    });
    let release: EventHandler<MouseEvent> = EventHandler::new({
        let id = toast_id.clone();
        move |_event| {
            let mut held = held;
            let mut ring = notify_target;
            release_toast_dismissal(&release_pump, &id, &mut held, &mut ring);
        }
    });
    let hold_focus: EventHandler<FocusEvent> = EventHandler::new({
        let id = toast_id.clone();
        let focus_target = target.clone();
        move |_event| {
            let mut held = held;
            let mut ring = notify_target;
            hold_toast_dismissal(
                &focus_pump,
                &id,
                &mut held,
                &mut ring,
                hoverable,
                focus_target.as_deref(),
            );
        }
    });
    let release_focus: EventHandler<FocusEvent> = EventHandler::new({
        let id = toast_id.clone();
        move |_event| {
            let mut held = held;
            let mut ring = notify_target;
            release_toast_dismissal(&focus_release_pump, &id, &mut held, &mut ring);
        }
    });

    // Dismiss THEN act, in that order and through one call: revealing first
    // would leave a card on screen pointing at the session the user just left.
    let reveal: EventHandler<MouseEvent> = EventHandler::new({
        let pump = pump.clone();
        let id = toast_id.clone();
        let mut notify_target = notify_target;
        move |_event: MouseEvent| {
            let action = write_store(&pump, |store| take_toast_action(store, &id));
            let Some(action) = action else {
                return;
            };
            notify_target.clear(&id);
            match action.intent {
                ToastIntent::RevealSession { session_id } => {
                    let href = {
                        let core = pump.core();
                        let core = core.borrow();
                        session_by_id(core.store(), &session_id)
                            .map(|session| terminal_href(core.store(), session))
                    };
                    navigate.call(href.unwrap_or_else(|| format!("/s/{session_id}")));
                }
                ToastIntent::CopyText { text } => {
                    let pump = pump.clone();
                    let session_id = id.subject.clone();
                    // Called inside the click, which is the gesture the
                    // browser refused the original write for.
                    copy_text_then(&text, move |accepted| {
                        if accepted {
                            super::terminal_clipboard::raise_copied_toast(&pump, &session_id);
                        }
                    });
                }
                ToastIntent::OpenPairApprovals => {
                    navigate.call(crate::routes::Route::Pair.to_path())
                }
            }
        }
    });

    let dismiss: EventHandler<MouseEvent> = EventHandler::new({
        let pump = pump.clone();
        let id = toast_id.clone();
        let mut notify_target = notify_target;
        move |_event: MouseEvent| {
            notify_target.clear(&id);
            write_store(&pump, |store| dismiss_toast(store, &id));
        }
    });

    let copy_text = match &toast.details {
        Some(detail) => format!("{}\n{detail}", toast.msg),
        None => toast.msg.clone(),
    };
    // A toast replaced under its id keeps this card with new text, so the
    // confirmation belongs to the text it copied. A refusal raises none, and
    // the card text stays selectable.
    let copy_confirmed = copied() && copied_text.read().as_deref() == Some(copy_text.as_str());
    let copy: EventHandler<MouseEvent> = EventHandler::new(move |_event: MouseEvent| {
        copied_text.set(Some(copy_text.clone()));
        copy_to_clipboard(&copy_text);
    });

    let action = rsx! {
        ToastActionRow {
            label: toast.action.as_ref().map(|action| action.label.clone()),
            // Copy is noise on a three-word success line; it earns its slot only
            // where there is output worth keeping.
            show_copy: toast.kind == ToastKind::Err || toast.details.is_some(),
            copied: copy_confirmed,
            compact,
            on_reveal: reveal,
            on_copy: copy,
            on_dismiss: dismiss,
        }
    };

    let kind_name = toast.kind.as_str();
    let headline = toast.msg.clone();
    let details = toast.details.clone();
    let is_error = toast.kind == ToastKind::Err;
    let window_ms = toast.kind.default_ttl_ms();
    let held_now = held();
    let play_state = if held_now { "paused" } else { "running" };
    let color = toast_color(toast.kind);

    rsx! {
        Box {
            sx: entrance_sx,
            "data-testid": "toast",
            "data-kind": kind_name,
            "data-compact": if compact { "true" } else { "false" },
            "data-dismiss-held": held_now.then_some("true"),
            onmouseenter: move |event: MouseEvent| hold.call(event),
            onmouseleave: move |event: MouseEvent| release.call(event),
            onfocusin: move |event: FocusEvent| hold_focus.call(event),
            onfocusout: move |event: FocusEvent| release_focus.call(event),
            Paper {
                shadow: "md", radius: "md", bordered: true, sx: card_sx,
                role: if is_error { "alert" } else { "status" },
                aria_live: if is_error { "assertive" } else { "polite" },
                aria_atomic: "true",
                Flex { direction: "row", align: "start", gap: "xs", wrap: false,
                    Indicator { color, aria_hidden: "true" }
                    Text { size: "sm", sx: headline_sx, title: headline.clone(), "{headline}" }
                    if !compact { {action.clone()} }
                }
                if let Some(detail) = details.as_ref() {
                    Text { component: "pre", size: "xs", color: "muted",
                        "data-testid": "toast-details", sx: detail_sx, "{detail}"
                    }
                }
                if compact {
                    Flex { direction: "row", justify: "end", align: "center", gap: "xs", {action} }
                }
                if let Some(window) = window_ms {
                    Text { component: "span", color, aria_hidden: "true", sx: countdown_sx,
                        style: "animation-duration:{window}ms;animation-play-state:{play_state};",
                    }
                }
            }
        }
    }
}

/// Freeze this card's auto-dismiss window and ring the session it points at.
/// A card that is not hoverable never holds: a touch tap has no matching leave,
/// so a hold taken there would pin the dismissal until the reader pressed ✕.
fn hold_toast_dismissal(
    pump: &Pump,
    id: &ToastId,
    held: &mut Signal<bool>,
    ring: &mut NotifyTarget,
    hoverable: bool,
    target: Option<&str>,
) {
    if !hoverable {
        return;
    }
    held.set(true);
    ring.ring(id, target);
    write_store(pump, |store| hold_toast_dismiss(store, id, now_ms()));
}

/// Release the hold and drop the ring.
fn release_toast_dismissal(
    pump: &Pump,
    id: &ToastId,
    held: &mut Signal<bool>,
    ring: &mut NotifyTarget,
) {
    held.set(false);
    ring.clear(id);
    write_store(pump, |store| release_toast_dismiss(store, id, now_ms()));
}
