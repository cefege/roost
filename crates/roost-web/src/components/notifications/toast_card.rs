//! One toast surface inside the notification dock: the kind presentation, the
//! compact two-row action layout, the copy affordance for error and detail
//! text, and the hover/focus hold that freezes auto-dismiss and rings the toast's
//! target session. Rendered by `toast_stack`; identity, window and dismissal
//! are `roost_client_core::store::toasts`, read here rather than reimplemented.
//! Ports `apps/web/src/components/notifications/ToastCard.tsx`.

use dioxus::prelude::*;
use roost_client_core::store::selectors::session_by_id;
use roost_client_core::store::toasts::{
    ToastId, ToastIntent, ToastKind, dismiss_toast, hold_toast_dismiss, release_toast_dismiss,
    take_toast_action,
};

use super::clipboard;
use super::notify_target::NotifyTarget;
use super::store_write::write_store;
use crate::components::deck::deck_dom;
use crate::components::layout::window_size::use_is_compact;
use crate::components::md::{
    Button, ButtonSize, ButtonVariant, IconButton, IconButtonSize, StatusDot, Surface,
    SurfaceRadius,
};
use crate::components::terminal::dom::{now_ms, sleep_ms};
use crate::pump::{Pump, use_store};
use crate::router_state::use_navigate;
use crate::terminal_href::terminal_href;

/// How long the Copy label reads "Copied" before it returns to "Copy".
const COPIED_MS: u64 = 1_500;

/// The `StatusDot` spelling each kind reads as.
const fn dot_status(kind: ToastKind) -> &'static str {
    match kind {
        ToastKind::Ok => "ok",
        ToastKind::Warn => "warn",
        ToastKind::Err => "error",
    }
}

/// The accent the countdown bar drains in.
const fn accent(kind: ToastKind) -> &'static str {
    match kind {
        ToastKind::Ok => "var(--status-ok)",
        ToastKind::Warn => "var(--status-warn)",
        ToastKind::Err => "var(--md-sys-color-error)",
    }
}

/// The message, its window and its actions, read from the store on every render.
#[component]
pub fn ToastCard(toast_id: ToastId) -> Element {
    let pump = use_store();
    let navigate = use_navigate();
    let compact = use_is_compact();
    let notify_target = use_context::<NotifyTarget>();
    let mut copied = use_signal(|| false);
    let mut copy_generation = use_signal(|| 0_u32);
    let held = use_signal(|| false);

    // The only reactive input is the copy generation, so the confirmation
    // window restarts exactly once per successful copy and never per render.
    use_future(move || {
        let _generation = copy_generation.read();
        async move {
            sleep_ms(COPIED_MS).await;
            copied.set(false);
        }
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
                    clipboard::copy_text_then(&text, move |accepted| {
                        if accepted {
                            super::terminal_clipboard::raise_copied_toast(&pump, &session_id);
                        }
                    });
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
    let copy: EventHandler<MouseEvent> = EventHandler::new(move |_event: MouseEvent| {
        // A denial leaves the label unchanged: the card text stays selectable.
        if !clipboard::copy_text(&copy_text) {
            return;
        }
        copied.set(true);
        copy_generation += 1;
    });

    let action = rsx! {
        ToastActionRow {
            label: toast.action.as_ref().map(|action| action.label.clone()),
            // Copy is noise on a three-word success line; it earns its slot only
            // where there is output worth keeping.
            show_copy: toast.kind == ToastKind::Err || toast.details.is_some(),
            copied,
            button_size: if compact { ButtonSize::Sm } else { ButtonSize::Xs },
            icon_size: if compact { IconButtonSize::IconSm } else { IconButtonSize::IconXs },
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
    let accent_color = accent(toast.kind);
    let dot = dot_status(toast.kind).to_owned();

    rsx! {
        div {
            class: "roost-toast-slot",
            "data-testid": "toast",
            "data-kind": kind_name,
            "data-compact": if compact { "true" } else { "false" },
            "data-dismiss-held": held_now.then_some("true"),
            style: "user-select: text;",
            onmouseenter: move |event: MouseEvent| hold.call(event),
            onmouseleave: move |event: MouseEvent| release.call(event),
            onfocusin: move |event: FocusEvent| hold_focus.call(event),
            onfocusout: move |event: FocusEvent| release_focus.call(event),
            Surface {
                level: 2,
                elevation: 3,
                radius: SurfaceRadius::Md,
                border: true,
                class: "roost-toast",
                role: Some(if is_error { "alert".to_owned() } else { "status".to_owned() }),
                aria_live: Some(if is_error { "assertive" } else { "polite" }.to_owned()),
                aria_atomic: Some("true".to_owned()),
                style: "display: flex; flex-direction: column; gap: var(--md-space-2); padding: var(--md-space-3); color: var(--md-sys-color-on-surface); white-space: pre-wrap; word-break: break-word;".to_owned(),
                div {
                    style: "display: flex; align-items: flex-start; gap: var(--md-space-2);",
                    StatusDot { status: dot }
                    span {
                        class: "md-body-m",
                        title: headline.clone(),
                        style: "flex: 1; min-width: 0; user-select: text; display: -webkit-box; -webkit-line-clamp: 4; -webkit-box-orient: vertical; overflow: hidden;",
                        "{headline}"
                    }
                    if !compact {
                        {action.clone()}
                    }
                }
                if let Some(detail) = details.as_ref() {
                    pre {
                        "data-testid": "toast-details",
                        class: "md-body-s",
                        style: "margin: 0; padding: var(--md-space-2); background: var(--md-sys-color-surface-container-highest); border-radius: var(--md-shape-xs); font-family: var(--term-font-family); color: var(--md-sys-color-on-surface-variant); white-space: pre-wrap; word-break: break-word; max-height: calc(var(--md-space-9) * 5); overflow: auto; user-select: text;",
                        "{detail}"
                    }
                }
                if compact {
                    div {
                        style: "display: flex; justify-content: flex-end; align-items: center; gap: var(--md-space-2);",
                        {action}
                    }
                }
                if let Some(window) = window_ms {
                    span {
                        "aria-hidden": "true",
                        style: format!(
                            "position: absolute; inset: auto 0 0; height: var(--workbench-border-width); background: {accent_color}; transform-origin: left center; animation: roost-toast-countdown {window}ms linear forwards; animation-play-state: {};",
                            if held_now { "paused" } else { "running" },
                        ),
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

/// The card's three actions in v2's order. Every one stops the card's hover
/// handlers, so pressing a button is not read as a pointer that arrived and
/// left — which would release the hold it just needed.
#[component]
fn ToastActionRow(
    label: Option<String>,
    show_copy: bool,
    copied: Signal<bool>,
    button_size: ButtonSize,
    icon_size: IconButtonSize,
    on_reveal: EventHandler<MouseEvent>,
    on_copy: EventHandler<MouseEvent>,
    on_dismiss: EventHandler<MouseEvent>,
) -> Element {
    let copied_now = copied();
    rsx! {
        if let Some(label) = label.as_ref() {
            Button {
                variant: ButtonVariant::Ghost,
                size: button_size,
                title: label.clone(),
                style: "flex-shrink: 0;",
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
                title: if copied_now { "Copied to clipboard" } else { "Copy full message" },
                style: "flex-shrink: 0;",
                onclick: move |event: MouseEvent| {
                    event.stop_propagation();
                    on_copy.call(event);
                },
                if copied_now { "Copied" } else { "Copy" }
            }
        }
        IconButton {
            icon: "close",
            label: "Dismiss",
            title: "Dismiss",
            size: icon_size,
            onclick: move |event: MouseEvent| {
                event.stop_propagation();
                on_dismiss.call(event);
            },
        }
    }
}
