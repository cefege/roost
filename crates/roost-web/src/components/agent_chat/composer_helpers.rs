//! Shared focus, draft and stop mechanics for the composer.
//! These helpers keep the mounted input and the harness abort path consistent.
//! Called by the composer for menu selection, field focus and run interruption.

use std::rc::Rc;

#[cfg(target_arch = "wasm32")]
use crate::pump::Pump;
use dioxus::prelude::*;

#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::AbortAgentChat;

pub(super) fn fill_draft(
    mut draft: Signal<String>,
    mut selected_option: Signal<usize>,
    text: &str,
) {
    draft.set(text.to_string());
    selected_option.set(0);
}

pub(super) fn focus_field(field: Signal<Option<Rc<MountedData>>>) {
    let Some(handle) = field.peek().clone() else {
        return;
    };
    spawn(async move {
        let _ = handle.set_focus(true).await;
    });
}

#[cfg(target_arch = "wasm32")]
pub(super) fn abort_run(pump: Pump, conversation_id: String) {
    wasm_bindgen_futures::spawn_local(async move {
        let request_id = super::request_ids::new_request_id();
        let abort = AbortAgentChat {
            conversation_id,
            request_id,
        };
        if let Err(error) = pump.rpc().call(&abort).await {
            tracing::warn!(target: "agent_chat", %error, "agent abort failed");
        }
    });
}
