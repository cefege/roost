//! How the chat surface catches its transcript up: the missing event batches
//! after the loaded cursor first, a full snapshot when they were pruned or do
//! not line up. Called by `agent_chat::surface`; talks to the coordinator
//! through the pump's Connect client and installs results as store intents.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::client::agent_chat::AgentChatIntent;
use roost_client_core::client::rpc::ConnectCode;
use roost_client_core::client::rpc::calls::agent_chat::{GetAgentChatEvents, GetAgentChatSnapshot};

/// How long a failed snapshot fetch waits before it is tried again.
const SNAPSHOT_RETRY_MS: u64 = 2_000;

pub(super) fn fetch_snapshot(
    pump: crate::pump::Pump,
    conversation_id: String,
    mut not_found: Signal<bool>,
    mut fetch_attempt: Signal<u32>,
    mut snapshot_generation: Signal<u64>,
) {
    not_found.set(false);
    // `peek`: the surface calls this from inside an effect, and a tracked
    // read there would re-run that effect on the write below, forever.
    let generation = *snapshot_generation.peek() + 1;
    snapshot_generation.set(generation);
    pump.dispatch(ClientEvent::AgentChat(AgentChatIntent::SnapshotRequested {
        conversation_id: conversation_id.clone(),
        generation,
    }));
    wasm_bindgen_futures::spawn_local(async move {
        match pump
            .rpc()
            .call(&GetAgentChatSnapshot {
                conversation_id: conversation_id.clone(),
            })
            .await
        {
            Ok((seq, transcript)) => {
                pump.dispatch(ClientEvent::AgentChat(AgentChatIntent::SnapshotLoaded {
                    conversation_id,
                    generation,
                    seq,
                    transcript,
                }))
            }
            Err(error) if error.code() == Some(&ConnectCode::NotFound) => {
                if let Ok(mut gone) = not_found.try_write() {
                    *gone = true;
                }
            }
            Err(error) => {
                tracing::warn!(target: "agent_chat", %error, "transcript snapshot failed; retrying");
                crate::components::terminal::dom::sleep_ms(SNAPSHOT_RETRY_MS).await;
                if let Ok(mut attempt) = fetch_attempt.try_write() {
                    *attempt = attempt.wrapping_add(1);
                }
            }
        }
    });
}

pub(super) fn fetch_events_or_snapshot(
    pump: crate::pump::Pump,
    conversation_id: String,
    after_cursor: u64,
    not_found: Signal<bool>,
    fetch_attempt: Signal<u32>,
    snapshot_generation: Signal<u64>,
) {
    wasm_bindgen_futures::spawn_local(async move {
        let repaired = match pump
            .rpc()
            .call(&GetAgentChatEvents {
                conversation_id: conversation_id.clone(),
                after_cursor,
                limit: 500,
            })
            .await
        {
            Ok((batches, false)) if !batches.is_empty() && batches.len() < 500 => {
                let contiguous = batches.iter().enumerate().all(|(idx, (cursor, _))| {
                    after_cursor.checked_add(u64::try_from(idx).unwrap_or(u64::MAX) + 1)
                        == Some(*cursor)
                });
                if contiguous {
                    pump.dispatch(ClientEvent::AgentChat(AgentChatIntent::EventsLoaded {
                        conversation_id: conversation_id.clone(),
                        batches,
                        truncated: false,
                    }));
                    true
                } else {
                    false
                }
            }
            Ok(_) => false,
            Err(error) => {
                tracing::warn!(target: "agent_chat", %error, "transcript event repair failed");
                false
            }
        };
        if !repaired {
            fetch_snapshot(
                pump,
                conversation_id,
                not_found,
                fetch_attempt,
                snapshot_generation,
            );
        }
    });
}
