//! "Start agent here": create a built-in agent conversation in one folder on
//! one machine, then land on its chat. Called by the folder picker's controls
//! and the sidebar folder menu; depends on the `AgentChatCreate` UI-direct call.
//! Folder memory is not touched: it holds session ids and links `/s/…`.

use dioxus::prelude::*;

use crate::pump::Pump;

#[cfg(target_arch = "wasm32")]
use roost_client_core::ClientEvent;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::CreateAgentChat;
#[cfg(target_arch = "wasm32")]
use roost_client_core::store::shell_intent::ShellIntent;

/// Create a conversation whose tools start in `folder` on `worker_fp`, then
/// navigate to it. A refusal surfaces as a shell action failure.
pub fn launch_agent(pump: Pump, worker_fp: String, folder: String, navigate: EventHandler<String>) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(run_launch_agent(pump, worker_fp, folder, navigate));
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, worker_fp, folder, navigate);
}

#[cfg(target_arch = "wasm32")]
async fn run_launch_agent(
    pump: Pump,
    worker_fp: String,
    folder: String,
    navigate: EventHandler<String>,
) {
    let auth_generation = pump.core().borrow().store().auth_generation;
    let call = CreateAgentChat {
        worker_fp: worker_fp.clone(),
        cwd: folder.clone(),
        model: None,
    };
    let created = match pump.rpc().call(&call).await {
        Ok(created) => created,
        Err(error) => {
            let message = format!("New agent failed: {error}");
            tracing::warn!(target: "agent_chat", %message, "agent launch refused");
            pump.dispatch(ClientEvent::Shell(ShellIntent::ActionFailed { message }));
            return;
        }
    };
    if pump.core().borrow().store().auth_generation != auth_generation {
        return;
    }
    tracing::info!(
        target: "agent_chat",
        conversation_id = %created.id,
        worker_fp = %worker_fp,
        folder = %folder,
        "agent launch landed"
    );
    navigate.call(crate::routes::agent_href(&created.id));
}
