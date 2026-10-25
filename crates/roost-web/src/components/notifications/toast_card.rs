//! One toast surface inside the notification dock: the kind presentation, the
//! compact two-row action layout, the copy affordance for error and detail
//! text, and the hover/focus hold that freezes auto-dismiss and rings the toast's
//! target session. Rendered by `toast_stack`; identity, window and dismissal
//! are `roost_client_core::store::toasts`, read here rather than reimplemented.

use dioxus::prelude::*;
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
use crate::components::ui::{Button, ButtonSize, ButtonVariant, ToastPresentation, ToastType};
use crate::pump::{Pump, use_store};
use crate::router_state::use_navigate;
use crate::terminal_href::terminal_href;

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
            button_size: ButtonSize::Sm,
            on_reveal: reveal,
            on_copy: copy,
        }
    };

    let kind_name = toast.kind.as_str();
    let title = toast.msg.clone();
    let details = toast.details.clone();
    let is_error = toast.kind == ToastKind::Err;
    let window_ms = toast.kind.default_ttl_ms();
    let held_now = held();
    let play_state = if held_now { "paused" } else { "running" };
    let variant = match toast.kind {
        ToastKind::Ok => ToastType::Success,
        ToastKind::Warn => ToastType::Warning,
        ToastKind::Err => ToastType::Error,
    };

    rsx! {
        div {
            class: "animate-roost-toast-in motion-reduce:animate-none select-text",
            "data-testid": "toast",
            "data-kind": kind_name,
            "data-compact": if compact { "true" } else { "false" },
            "data-dismiss-held": held_now.then_some("true"),
            onmouseenter: move |event: MouseEvent| hold.call(event),
            onmouseleave: move |event: MouseEvent| release.call(event),
            onfocusin: move |event: FocusEvent| hold_focus.call(event),
            onfocusout: move |event: FocusEvent| release_focus.call(event),
            ToastPresentation {
                variant,
                title,
                role: Some(if is_error { "alert" } else { "status" }.to_owned()),
                aria_live: Some(if is_error { "assertive" } else { "polite" }.to_owned()),
                aria_atomic: Some("true".to_owned()),
                close_label: Some("Dismiss".to_owned()),
                on_close: dismiss,
                class: "relative w-full max-w-full min-w-0 cursor-default touch-auto overflow-hidden whitespace-pre-wrap [word-break:break-word]",
                progress: window_ms.map(|_| 1.0),
                progress_class: Some("animate-roost-countdown-drain motion-reduce:animate-none".to_owned()),
                progress_style: window_ms.map(|window| format!(
                    "animation-duration: {window}ms; animation-play-state: {play_state};"
                )),
                if let Some(detail) = details.as_ref() {
                    pre {
                        class: "m-0 mt-2 max-h-36 overflow-auto select-text bg-muted text-muted-foreground whitespace-pre-wrap [word-break:break-word] [font-family:var(--term-font-family)]",
                        "{detail}"
                    }
                }
                if toast.action.is_some() || toast.kind == ToastKind::Err || details.is_some() {
                    div { class: "mt-2 flex flex-wrap items-center justify-end gap-2", {action} }
                }
            }
        }
    }
}
#[component]
fn ToastActionRow(
    label: Option<String>,
    show_copy: bool,
    copied: bool,
    button_size: ButtonSize,
    on_reveal: EventHandler<MouseEvent>,
    on_copy: EventHandler<MouseEvent>,
) -> Element {
    rsx! {
        if let Some(label) = label.as_ref() {
            Button {
                variant: ButtonVariant::Ghost,
                size: button_size,
                title: label.clone(),
                class: "shrink-0",
                onclick: move |event: MouseEvent| {
                    event.stop_propagation();
                    on_reveal.call(event);
                },
                "{label}"
            }
        }
        if show_copy {
            Button {
                variant: ButtonVariant::Ghost,
                size: button_size,
                title: if copied { "Copied to clipboard" } else { "Copy full message" },
                class: "shrink-0",
                onclick: move |event: MouseEvent| {
                    event.stop_propagation();
                    on_copy.call(event);
                },
                if copied { "Copied" } else { "Copy" }
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
