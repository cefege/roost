//! Mounts one browser-owned view of the shared OMP terminal UI.
//! The mounted view owns its lease, viewport events, and close request while
//! OMP and the authenticated agent session remain authoritative.
#[path = "omp_ui_mount_appearance.rs"]
mod appearance;
use dioxus::prelude::*;
use roost_client_core::client::agent_chat::{OmpActionIdentity, OmpActionQueue};
use roost_protocol::wire::agent_chat::ConversationSummary;
use serde_json::{Value, json};
use std::cell::RefCell;
use std::rc::Rc;

use super::omp_interaction_card::OmpInteractionCard;
use super::omp_ui_mount_transport::{
    VIEW_LEASE_MS, apply_omp_receipts, current_dark_theme, dispatch_omp_ui_action,
    open_omp_ui_action, send_direct,
};
use super::semantic_ui::OmpCommandSurface;
use super::surface_state::assistant_is_streaming;
use super::working_row::{WorkingRow, working_label};
use crate::components::deck::deck_dom::{SizeWatch, Timeout};
use crate::components::hooks::use_media_query::use_media_query;
use crate::components::ui::{Button, ButtonSize, ButtonVariant, Card, CardSize};
use crate::pump::Pump;

#[cfg(target_arch = "wasm32")]
use super::omp_ui_mount_transport::{ThemeObserver, observe_dark_theme};

#[component]
pub(super) fn OmpUiMount(
    conversation: ConversationSummary,
    active: bool,
    host_connected: bool,
    pump: Pump,
) -> Element {
    let conversation_id = conversation.id.clone();
    let _revision = pump.revision()();
    let view_id = use_hook(super::request_ids::new_request_id);
    let mut local_error = use_signal(|| None::<String>);
    let mut input_failure = use_signal(|| None::<(u64, String)>);
    let mut opened_owner = use_signal(|| None::<String>);
    let mut retry = use_signal(|| 0_u32);
    let mut reset_requested = use_signal(|| false);
    let lease_tick = use_signal(|| 0_u32);
    let viewport = use_signal(|| None::<(u32, u32)>);
    let observer = use_hook(|| Rc::new(RefCell::new(None::<SizeWatch>)));
    let lease_timer = use_hook(|| Rc::new(RefCell::new(None::<Timeout>)));
    let reduce_motion = use_media_query("(prefers-reduced-motion: reduce)");
    let dark = use_signal(current_dark_theme);
    let action_queue = use_hook(|| Rc::new(RefCell::new(OmpActionQueue::new())));
    #[cfg(target_arch = "wasm32")]
    let theme_observer = use_hook(|| Rc::new(RefCell::new(None::<ThemeObserver>)));

    let (transport_id, loaded_seq) = {
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        (
            store
                .agent_chat
                .transcripts
                .get(&conversation_id)
                .and_then(|loaded| loaded.transcript.omp_ui_transport_id.clone())
                .filter(|transport| !transport.is_empty()),
            store.agent_chat.loaded_seq(&conversation_id).unwrap_or(0),
        )
    };
    let identity = OmpActionIdentity {
        conversation_id: conversation_id.clone(),
        view_id: view_id.clone(),
        transport_id: transport_id.clone().unwrap_or_default(),
    };
    // `use_drop` captures only the first render; close with the current identity.
    let mut latest_identity = use_hook(|| CopyValue::new(identity.clone()));
    latest_identity.set(identity.clone());
    let report_failure = use_callback(move |error: String| {
        let serial = input_failure
            .peek()
            .as_ref()
            .map_or(1, |(serial, _)| serial.wrapping_add(1));
        input_failure.set(Some((serial, error.clone())));
        local_error.set(Some(error));
    });
    let action_outcome = use_callback(move |result: Result<(), String>| {
        if let Err(error) = result {
            report_failure.call(error);
        }
    });

    {
        let request_pump = pump.clone();
        let request_identity = identity.clone();
        let request_outcome = action_outcome;
        use_effect(use_reactive(
            (
                &conversation_id,
                &active,
                &host_connected,
                &retry(),
                &reset_requested(),
                &viewport(),
                &dark(),
                &reduce_motion(),
            ),
            move |(
                conversation_id,
                active,
                host_connected,
                _retry,
                reset,
                viewport,
                dark,
                reduce,
            )| {
                if !active || !host_connected {
                    if opened_owner.peek().as_deref() == Some(conversation_id.as_str()) {
                        opened_owner.set(None);
                        send_direct(
                            &request_pump,
                            &request_identity,
                            json!({"method":"close","params":{}}),
                            request_outcome,
                        );
                    }
                    return;
                }
                if opened_owner.peek().as_deref() == Some(conversation_id.as_str()) {
                    return;
                }
                let Some((cols, rows)) = viewport else {
                    return;
                };
                opened_owner.set(Some(conversation_id.to_owned()));
                local_error.set(None);
                send_direct(
                    &request_pump,
                    &request_identity,
                    open_omp_ui_action(cols, rows, dark, reduce, reset),
                    EventHandler::new(move |result| {
                        if let Err(error) = result {
                            opened_owner.set(None);
                            local_error.set(Some(error));
                        } else {
                            reset_requested.set(false);
                        }
                    }),
                );
            },
        ));
    }
    appearance::use_omp_appearance(
        pump.clone(),
        identity.clone(),
        appearance::Appearance {
            active,
            opened_owner,
            dark,
            reduce_motion,
        },
        action_outcome,
    );

    {
        let request_pump = pump.clone();
        let request_identity = identity.clone();
        let request_outcome = action_outcome;
        use_effect(use_reactive(
            (&conversation_id, &active, &opened_owner(), &lease_tick()),
            move |(conversation_id, active, owner, tick)| {
                *lease_timer.borrow_mut() = None;
                if !active || owner.as_deref() != Some(conversation_id.as_str()) {
                    return;
                }
                let mut next_tick = lease_tick;
                *lease_timer.borrow_mut() = Timeout::after(VIEW_LEASE_MS, move || next_tick += 1);
                if tick > 0 {
                    let Some((cols, rows)) = viewport() else {
                        return;
                    };
                    send_direct(
                        &request_pump,
                        &request_identity,
                        open_omp_ui_action(cols, rows, dark(), reduce_motion(), false),
                        request_outcome,
                    );
                }
            },
        ));
    }

    {
        let receipt_pump = pump.clone();
        let receipt_queue = Rc::clone(&action_queue);
        let receipt_identity = identity.clone();
        let receipt_outcome = action_outcome;
        use_effect(use_reactive(
            (
                &conversation_id,
                &view_id,
                &transport_id,
                &loaded_seq,
                &_revision,
            ),
            move |(conversation_id, view_id, transport, loaded, _revision)| {
                receipt_queue
                    .borrow_mut()
                    .retain_identity(&receipt_identity);
                if transport.as_deref().is_none() {
                    receipt_queue.borrow_mut().cancel_all();
                    return;
                }
                receipt_pump.write_store(|store| {
                    roost_client_core::store::discard_omp_host_transports(
                        store,
                        &conversation_id,
                        &view_id,
                        transport.as_deref().unwrap_or_default(),
                    );
                });
                apply_omp_receipts(
                    receipt_pump.clone(),
                    Rc::clone(&receipt_queue),
                    &receipt_identity,
                    loaded,
                    receipt_outcome,
                    report_failure,
                );
            },
        ));
    }

    use_drop({
        let close_pump = pump.clone();
        let close_identity = latest_identity;
        let close_id = conversation_id.clone();
        let close_queue = Rc::clone(&action_queue);
        move || {
            close_queue.borrow_mut().cancel_all();
            if opened_owner.peek().as_deref() == Some(close_id.as_str()) {
                let identity = close_identity.read().clone();
                send_direct(
                    &close_pump,
                    &identity,
                    json!({"method":"close","params":{}}),
                    EventHandler::new(|_| {}),
                );
            }
        }
    });
    let core = pump.core();
    let core = core.borrow();
    let store = core.store();
    let Some(transcript) = store
        .agent_chat
        .transcripts
        .get(&conversation_id)
        .map(|loaded| &loaded.transcript)
    else {
        return rsx! {
            div { class: "flex min-h-0 flex-1 items-center justify-center", role: "status",
                p { class: "text-sm text-muted-foreground", "Loading OMP conversation…" }
            }
        };
    };

    let error = transcript.omp_ui_error.clone().or_else(&*local_error);
    let view_closed = transcript.omp_ui.as_ref().is_some_and(|document| {
        !document.surfaces.is_empty() && document.surfaces.values().all(|surface| surface.suspended)
    });
    let action_pump = pump.clone();
    let action_identity = identity.clone();
    let action_queue_for_send = Rc::clone(&action_queue);
    let action_error = action_outcome;
    let on_action = EventHandler::new(move |action: Value| {
        local_error.set(None);
        dispatch_omp_ui_action(
            action_pump.clone(),
            Rc::clone(&action_queue_for_send),
            action_identity.clone(),
            action,
            action_error,
        );
    });
    let size_pump = pump.clone();
    let size_identity = latest_identity;
    let size_outcome = action_outcome;
    let size_owner = opened_owner;
    let mut size_signal = viewport;
    let size_observer = Rc::clone(&observer);
    let resized = EventHandler::new(move |size: (u32, u32)| {
        let identity = size_identity.read().clone();
        if *size_signal.peek() != Some(size) {
            size_signal.set(Some(size));
            if size_owner.peek().as_deref() == Some(identity.conversation_id.as_str()) {
                send_direct(
                    &size_pump,
                    &identity,
                    json!({"method":"event","params":{"event":{"ev":"resize","cols":size.0,"rows":size.1}}}),
                    size_outcome,
                );
            }
        }
    });
    rsx! {
        div {
            class: "flex h-full min-h-0 flex-1 flex-col overflow-hidden",
            onmounted: move |event| {
                let mounted = event.data();
                *size_observer.borrow_mut() = super::omp_ui_mount_transport::observe_omp_viewport(mounted, resized);
                #[cfg(target_arch = "wasm32")]
                {
                    *theme_observer.borrow_mut() = observe_dark_theme(dark);
                }
            },
            if transcript.run_state == roost_protocol::wire::agent_chat::AgentRunState::Running
                || !transcript.omp_interactions.is_empty() {
                WorkingRow {
                    label: if !transcript.omp_interactions.is_empty() { "Waiting for your answer…".to_string() }
                        else if assistant_is_streaming(transcript) { "Writing…".to_string() }
                        else { working_label(&transcript.items) },
                }
            }
            for interaction in transcript.omp_interactions.iter().cloned() {
                OmpInteractionCard { key: "{interaction.id}", conversation_id: conversation_id.clone(), interaction, pump: pump.clone() }
            }
            if let Some(error) = error {
                div { role: "alert",
                    "data-omp-projection-error": transcript.omp_ui_error.as_ref().map(|_| "true"),
                    Card { size: CardSize::Sm, class: Some("flex items-center gap-3 p-3".into()),
                        p { class: "min-w-0 flex-1 text-sm text-destructive", "OMP UI request failed: {error}" }
                        Button {
                            size: ButtonSize::Sm,
                            variant: ButtonVariant::Secondary,
                            disabled: !host_connected,
                            onclick: move |_| {
                                local_error.set(None);
                                opened_owner.set(None);
                                reset_requested.set(true);
                                retry += 1;
                            },
                            "Reconnect"
                        }
                    }
                }
            }
            if !host_connected {
                div { class: "flex min-h-0 flex-1 items-center justify-center",
                    role: "status",
                    p { class: "text-sm text-muted-foreground", "Waiting for the OMP worker to reconnect…" }
                }
            } else if view_closed {
                div { class: "flex min-h-0 flex-1 flex-col items-center justify-center gap-3",
                    role: "status",
                    p { class: "text-sm text-muted-foreground", "OMP view closed. The conversation is retained." }
                    Button { variant: ButtonVariant::Secondary,
                        onclick: move |_| {
                            opened_owner.set(None);
                            reset_requested.set(true);
                            retry += 1;
                        },
                        "Reopen OMP"
                    }
                }
            } else if transcript.omp_ui.is_some()||transport_id.is_some() {
                OmpCommandSurface {
                    key:"{conversation_id}/{transport_id.as_deref().unwrap_or_default()}",
                    conversation_id: conversation_id.clone(),
                    view_id:view_id.clone(),
                    transport_id:transport_id.clone().unwrap_or_default(),
                    worker_fp: conversation.worker_fp.clone(),
                    cwd: conversation.cwd.clone(),
                    pump: pump.clone(),
                    input_failure:input_failure(),
                    on_action,
                }
            } else {
                div { class: "flex min-h-0 flex-1 items-center justify-center",
                    role: "status",
                    p { class: "text-sm text-muted-foreground", "Connecting to OMP…" }
                }
            }
        }
    }
}
