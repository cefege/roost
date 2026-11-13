//! Owns the browser view's open request and lease for one OMP generation.
//! An asynchronous result may only affect the exact attempt that issued it;
//! transcript generation changes invalidate the old owner before re-opening.

use dioxus::prelude::*;
use roost_client_core::client::agent_chat::OmpActionIdentity;
use serde_json::json;
use std::{cell::RefCell, rc::Rc};

use super::super::omp_ui_mount_transport::{VIEW_LEASE_MS, open_omp_ui_action, send_direct};
use crate::components::deck::deck_dom::Timeout;
use crate::pump::Pump;

pub(super) type ViewOwner = (OmpActionIdentity, u64);

pub(super) fn owns(owner: &Option<ViewOwner>, identity: &OmpActionIdentity) -> bool {
    owner.as_ref().is_some_and(|(opened, _)| opened == identity)
}

fn current_attempt(
    owner: &Option<ViewOwner>,
    latest: &OmpActionIdentity,
    identity: &OmpActionIdentity,
    serial: u64,
) -> bool {
    latest == identity
        && owner
            .as_ref()
            .is_some_and(|(opened, attempt)| opened == identity && *attempt == serial)
}

pub(super) fn scoped_outcome(
    identity: OmpActionIdentity,
    latest: CopyValue<OmpActionIdentity>,
    failure: EventHandler<String>,
) -> EventHandler<Result<(), String>> {
    EventHandler::new(move |result| {
        if *latest.read() == identity
            && let Err(error) = result
        {
            failure.call(error);
        }
    })
}

#[derive(Clone, Copy)]
pub(super) struct ConnectionState {
    pub active: bool,
    pub host_connected: bool,
    pub retry: Signal<u32>,
    pub reset_requested: Signal<bool>,
    pub viewport: Signal<Option<(u32, u32)>>,
    pub dark: Signal<bool>,
    pub reduce_motion: ReadSignal<bool>,
    pub local_error: Signal<Option<String>>,
    pub latest_identity: CopyValue<OmpActionIdentity>,
}

pub(super) fn use_omp_connection(
    pump: Pump,
    identity: OmpActionIdentity,
    mut state: ConnectionState,
    failure: EventHandler<String>,
) -> Signal<Option<ViewOwner>> {
    let mut owner = use_signal(|| None::<ViewOwner>);
    let mut attempt = use_hook(|| CopyValue::new(0_u64));
    let mut failed = use_hook(|| CopyValue::new(None::<(OmpActionIdentity, u32)>));
    let lease_tick = use_signal(|| 0_u32);
    let lease_timer = use_hook(|| Rc::new(RefCell::new(None::<Timeout>)));
    {
        let pump = pump.clone();
        use_effect(use_reactive(
            (
                &identity,
                &state.active,
                &state.host_connected,
                &(state.retry)(),
                &(state.viewport)(),
                &(state.dark)(),
                &(state.reduce_motion)(),
            ),
            move |(identity, active, host_connected, retry, viewport, dark, reduce)| {
                if *state.latest_identity.read() != identity {
                    return;
                }
                if !active || !host_connected {
                    failed.set(None);
                    let previous = owner.peek().clone();
                    if let Some((previous, _)) = previous {
                        owner.set(None);
                        if previous == identity && !previous.transport_id.is_empty() {
                            let outcome =
                                scoped_outcome(previous.clone(), state.latest_identity, failure);
                            send_direct(
                                &pump,
                                &previous,
                                json!({"method":"close","params":{}}),
                                outcome,
                            );
                        }
                    }
                    return;
                }
                if owns(&owner.peek(), &identity) {
                    return;
                }
                if failed.read().as_ref() == Some(&(identity.clone(), retry)) {
                    return;
                }
                let Some((cols, rows)) = viewport else {
                    owner.set(None);
                    return;
                };
                let serial = attempt.read().wrapping_add(1);
                attempt.set(serial);
                failed.set(None);
                owner.set(Some((identity.clone(), serial)));
                let reset = *state.reset_requested.peek();
                state.local_error.set(None);
                state.reset_requested.set(false);
                let request_identity = identity.clone();
                send_direct(
                    &pump,
                    &identity,
                    open_omp_ui_action(cols, rows, dark, reduce, reset),
                    EventHandler::new(move |result| {
                        if !current_attempt(
                            &owner.peek(),
                            &state.latest_identity.read(),
                            &request_identity,
                            serial,
                        ) {
                            return;
                        }
                        if let Err(error) = result {
                            failed.set(Some((request_identity.clone(), retry)));
                            owner.set(None);
                            state.local_error.set(Some(error));
                        }
                    }),
                );
            },
        ));
    }
    {
        let pump = pump.clone();
        use_effect(use_reactive(
            (
                &identity,
                &state.active,
                &state.host_connected,
                &owner(),
                &lease_tick(),
            ),
            move |(identity, active, host_connected, current_owner, tick)| {
                *lease_timer.borrow_mut() = None;
                if !active
                    || !host_connected
                    || *state.latest_identity.read() != identity
                    || !owns(&current_owner, &identity)
                {
                    return;
                }
                let mut next_tick = lease_tick;
                *lease_timer.borrow_mut() = Timeout::after(VIEW_LEASE_MS, move || next_tick += 1);
                if tick > 0 && !identity.transport_id.is_empty() {
                    let outcome = scoped_outcome(identity.clone(), state.latest_identity, failure);
                    let Some((cols, rows)) = (state.viewport)() else {
                        return;
                    };
                    send_direct(
                        &pump,
                        &identity,
                        open_omp_ui_action(
                            cols,
                            rows,
                            (state.dark)(),
                            (state.reduce_motion)(),
                            false,
                        ),
                        outcome,
                    );
                }
            },
        ));
    }
    owner
}

#[cfg(test)]
mod tests {
    use super::*;
    use roost_client_core::client::agent_chat::{OmpActionQueue, OmpSubmit};

    fn identity(transport: &str) -> OmpActionIdentity {
        OmpActionIdentity {
            conversation_id: "conversation".into(),
            view_id: "view".into(),
            transport_id: transport.into(),
        }
    }

    #[test]
    fn generation_replacement_fences_late_opens_and_queued_input() {
        let first = identity("");
        let live = identity("generation-one");
        let replacement = identity("generation-two");
        let mut owner = Some((first.clone(), 1));
        assert!(current_attempt(&owner, &first, &first, 1));
        assert!(!current_attempt(&owner, &live, &first, 1));
        owner = Some((live.clone(), 2));
        assert!(!current_attempt(&owner, &live, &first, 1));
        assert!(current_attempt(&owner, &live, &live, 2));

        let mut queue = OmpActionQueue::new();
        assert!(matches!(
            queue.submit(
                "input-1".into(),
                live.clone(),
                json!({"method":"input","params":{"data":"one"}})
            ),
            OmpSubmit::Send(_)
        ));
        assert!(matches!(
            queue.submit(
                "input-2".into(),
                live.clone(),
                json!({"method":"input","params":{"data":"two"}})
            ),
            OmpSubmit::Queued
        ));
        owner = Some((replacement.clone(), 3));
        queue.retain_identity(&replacement);
        assert_eq!(queue.pending_count(), 0);
        assert!(!current_attempt(&owner, &replacement, &live, 2));
        assert!(current_attempt(&owner, &replacement, &replacement, 3));
        assert!(matches!(
            queue.submit(
                "input-3".into(),
                replacement.clone(),
                json!({"method":"input","params":{"data":"three"}})
            ),
            OmpSubmit::Send(_)
        ));
        assert!(!current_attempt(
            &Some((replacement.clone(), 4)),
            &replacement,
            &replacement,
            3
        ));
    }
}
