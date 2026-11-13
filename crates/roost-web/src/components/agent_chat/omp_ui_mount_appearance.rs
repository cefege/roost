//! Mirrors browser theme and motion into the mounted native view.
//! The mount supplies ownership and lease state; only changed appearance
//! values become OMP events, never new command policy.
use crate::pump::Pump;
use dioxus::prelude::*;
use roost_client_core::client::agent_chat::OmpActionIdentity;
use serde_json::json;
use std::{cell::RefCell, rc::Rc};

pub(super) struct Appearance {
    pub active: bool,
    pub opened_owner: Signal<Option<super::connection::ViewOwner>>,
    pub dark: Signal<bool>,
    pub reduce_motion: ReadSignal<bool>,
    pub latest_identity: CopyValue<OmpActionIdentity>,
}

pub(super) fn use_omp_appearance(
    pump: Pump,
    identity: OmpActionIdentity,
    state: Appearance,
    failure: EventHandler<String>,
) {
    let previous = use_hook(|| Rc::new(RefCell::new(None::<(bool, bool)>)));
    use_effect(use_reactive(
        (
            &identity,
            &state.active,
            &(state.opened_owner)(),
            &(state.dark)(),
            &(state.reduce_motion)(),
        ),
        move |(identity, active, owner, dark, reduce)| {
            let mut previous = previous.borrow_mut();
            if !active
                || identity.transport_id.is_empty()
                || !super::connection::owns(&owner, &identity)
            {
                *previous = None;
                return;
            }
            let changed = previous.replace((dark, reduce));
            drop(previous);
            let outcome =
                super::connection::scoped_outcome(identity.clone(), state.latest_identity, failure);
            if let Some((previous_dark, previous_reduce)) = changed {
                if previous_dark != dark {
                    super::send_direct(
                        &pump,
                        &identity,
                        json!({"method":"event","params":{"event":{"ev":"theme","dark":dark}}}),
                        outcome,
                    );
                }
                if previous_reduce != reduce {
                    super::send_direct(
                        &pump,
                        &identity,
                        json!({"method":"event","params":{"event":{"ev":"motion","reduce":reduce}}}),
                        outcome,
                    );
                }
            }
        },
    ));
}
