//! Document-owned sequencing for complete agent-settings snapshots.
//! Views update immediately, but RPCs and their completion work cannot overtake
//! one another. Loaders wait for pending writes when Settings is reopened.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::AgentSettingsView;

use super::Pump;

#[cfg(target_arch = "wasm32")]
use futures::FutureExt;
#[cfg(target_arch = "wasm32")]
use futures::future::{LocalBoxFuture, Shared};

#[cfg(target_arch = "wasm32")]
#[derive(Default)]
pub(super) struct SettingsWrites {
    tail: Option<Shared<LocalBoxFuture<'static, ()>>>,
    confirmed: Option<AgentSettingsView>,
    generation: u64,
}

impl Pump {
    pub(crate) fn save_agent_settings(
        &self,
        updated: AgentSettingsView,
        state: Signal<AgentSettingsView>,
        error: Signal<String>,
    ) {
        #[cfg(target_arch = "wasm32")]
        {
            use roost_client_core::client::rpc::calls::agent_chat::SetAgentSettings;

            let mut state = state;
            let mut error = error;
            let (generation, previous) = {
                let mut writes = self.inner.agent_settings_writes.borrow_mut();
                if writes
                    .tail
                    .as_ref()
                    .is_none_or(|tail| tail.peek().is_some())
                {
                    writes.confirmed = Some(state.peek().clone());
                }
                writes.generation = writes.generation.wrapping_add(1);
                (writes.generation, writes.tail.take())
            };
            // An event's caller may still hold a settings read until it returns.
            let optimistic = updated.clone();
            let optimistic_pump = self.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let latest = optimistic_pump
                    .inner
                    .agent_settings_writes
                    .borrow()
                    .generation
                    == generation;
                if latest {
                    if let Ok(mut state) = state.try_write() {
                        *state = optimistic;
                    }
                    if let Ok(mut error) = error.try_write() {
                        error.clear();
                    }
                }
            });
            let pump = self.clone();
            let pending = async move {
                if let Some(previous) = previous {
                    previous.await;
                }
                let result = match serde_json::to_string(&updated) {
                    Ok(settings_json) => pump
                        .rpc()
                        .call(&SetAgentSettings { settings_json })
                        .await
                        .map_err(|failure| format!("Could not save agent settings: {failure}")),
                    Err(failure) => Err(format!("Could not encode agent settings: {failure}")),
                };
                match result {
                    Ok(()) => {
                        let latest = {
                            let mut writes = pump.inner.agent_settings_writes.borrow_mut();
                            writes.confirmed = Some(updated);
                            writes.generation == generation
                        };
                        if latest && let Ok(mut error) = error.try_write() {
                            error.clear();
                        }
                    }
                    Err(failure) => {
                        let rollback = {
                            let writes = pump.inner.agent_settings_writes.borrow();
                            (writes.generation == generation)
                                .then(|| writes.confirmed.clone())
                                .flatten()
                        };
                        if let Some(confirmed) = rollback {
                            if let Ok(mut state) = state.try_write() {
                                *state = confirmed;
                            }
                            if let Ok(mut error) = error.try_write() {
                                *error = failure;
                            }
                        }
                    }
                }
            }
            .boxed_local()
            .shared();
            self.inner.agent_settings_writes.borrow_mut().tail = Some(pending.clone());
            wasm_bindgen_futures::spawn_local(pending);
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = (self, updated, state);
            let mut error = error;
            error.set("Agent settings are available in the web client.".to_owned());
        }
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) async fn wait_for_agent_settings_writes(&self) {
        let pending = self.inner.agent_settings_writes.borrow().tail.clone();
        if let Some(pending) = pending {
            pending.await;
        }
    }
}
