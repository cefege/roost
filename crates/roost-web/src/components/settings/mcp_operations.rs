//! MCP relay reads and mutations, including their refused-response policy.
//! `mcp` owns presentation and view state; these operations keep RPC effects
//! outside the component tree.

use dioxus::prelude::Signal;
#[cfg(target_arch = "wasm32")]
use dioxus::prelude::WritableExt;

use super::McpView;
use crate::pump::Pump;

/// Re-read the registry. A refusal is shown, never swallowed.
pub(super) fn reload(pump: Pump, view: Signal<McpView>) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        use roost_client_core::client::rpc::calls::settings::mcp::ListMcpRelays;

        let mut view = view;
        match pump.rpc().call(&ListMcpRelays).await {
            Ok(mut rows) => {
                rows.sort_by_key(|relay| relay.created_at_ms);
                view.write().relays = rows;
                view.write().load_error = None;
            }
            Err(error) => {
                tracing::warn!(target: "settings", %error, "mcp relay list refused");
                view.write().load_error = Some(error.to_string());
            }
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, view);
}

/// Register one relay, then re-read what the coordinator kept.
pub(super) fn create_relay(
    pump: Pump,
    label: String,
    kind: String,
    config_json: String,
    view: Signal<McpView>,
) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        use roost_client_core::ClientEvent;
        use roost_client_core::client::rpc::calls::settings::mcp::CreateMcpRelay;
        use roost_client_core::store::shell_intent::ShellIntent;

        let request = CreateMcpRelay {
            label,
            kind,
            config_json,
        };
        if let Err(error) = pump.rpc().call(&request).await {
            pump.dispatch(ClientEvent::Shell(ShellIntent::ActionFailed {
                message: format!("Add failed: {error}"),
            }));
            return;
        }
        reload(pump.clone(), view);
        pump.dispatch(ClientEvent::Shell(ShellIntent::ActionSucceeded {
            message: "Relay added".to_owned(),
        }));
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, label, kind, config_json, view);
}

/// Drop one relay, and say so when the coordinator will not.
pub(super) fn remove_relay(pump: Pump, id: String, view: Signal<McpView>) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        use roost_client_core::ClientEvent;
        use roost_client_core::client::rpc::calls::settings::mcp::DeleteMcpRelay;
        use roost_client_core::store::shell_intent::ShellIntent;

        let mut view = view;
        let request = DeleteMcpRelay { id: id.clone() };
        match pump.rpc().call(&request).await {
            Ok(true) => {}
            Ok(false) => {
                let message = "the coordinator has no such relay".to_owned();
                tracing::warn!(target: "settings", %message, "relay delete refused");
                view.write().delete_error = Some(message.clone());
                pump.dispatch(ClientEvent::Shell(ShellIntent::ActionFailed {
                    message: format!("Delete failed: {message}"),
                }));
                return;
            }
            Err(error) => {
                view.write().delete_error = Some(error.to_string());
                pump.dispatch(ClientEvent::Shell(ShellIntent::ActionFailed {
                    message: format!("Delete failed: {error}"),
                }));
                return;
            }
        }
        view.write().delete_error = None;
        view.write()
            .relays
            .retain(|relay| relay.id.to_string() != id);
        pump.dispatch(ClientEvent::Shell(ShellIntent::ActionSucceeded {
            message: "Relay removed".to_owned(),
        }));
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, id, view);
}
