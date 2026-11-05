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
    pub opened_owner: Signal<Option<String>>,
    pub dark: Signal<bool>,
    pub reduce_motion: ReadSignal<bool>,
}

pub(super) fn use_omp_appearance(
    pump: Pump,
    identity: OmpActionIdentity,
    state: Appearance,
    outcome: EventHandler<Result<(), String>>,
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
            if !active || owner.as_deref() != Some(identity.conversation_id.as_str()) {
                *previous = None;
                return;
            }
            let changed = previous.replace((dark, reduce));
            drop(previous);
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
