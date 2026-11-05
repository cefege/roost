//! The agent chat route loads transcript state and mounts the active OMP view.
//! Native conversations remain archived; the browser-owned view reads the live
//! semantic document directly from the store without copying the transcript.

use dioxus::prelude::*;

use super::last_conversation::use_remember_conversation;
use super::omp_ui_mount::OmpUiMount;
use crate::components::ui::{EmptyState, IconType, Skeleton};
use crate::pump::use_store;

const ROOT_CLASS: &str =
    "relative flex h-full w-full min-h-0 min-w-0 flex-col bg-background text-foreground";
const TRANSCRIPT_CLASS: &str = "min-h-0 flex-1 overflow-y-auto overscroll-y-contain py-5";

#[cfg(target_arch = "wasm32")]
use super::surface_fetch::{fetch_events_or_snapshot, fetch_snapshot};
#[cfg(target_arch = "wasm32")]
use roost_client_core::ClientEvent;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::agent_chat::AgentChatIntent;

#[component]
pub fn AgentChatSurface(conversation_id: String, active: bool) -> Element {
    let pump = use_store();
    let _revision = pump.revision()();
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_variables))]
    let (conversation, transcript_loaded, _stale, loaded_seq, host_connected) =
        read_store(&pump, &conversation_id);
    let not_found = use_signal(|| false);
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_variables))]
    let fetch_attempt = use_signal(|| 0_u32);
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_variables))]
    let snapshot_generation = use_signal(|| 0_u64);
    use_remember_conversation(
        &conversation_id,
        active,
        conversation.as_ref().is_some_and(|row| !row.is_native()),
    );
    #[cfg(target_arch = "wasm32")]
    {
        let fetch_needed = !transcript_loaded || _stale;
        let snapshot_pump = pump.clone();
        use_effect(use_reactive(
            (
                &conversation_id,
                &fetch_needed,
                &fetch_attempt(),
                &loaded_seq,
            ),
            move |(conversation_id, fetch_needed, _attempt, loaded_seq)| {
                if fetch_needed {
                    if transcript_loaded {
                        fetch_events_or_snapshot(
                            snapshot_pump.clone(),
                            conversation_id,
                            loaded_seq,
                            not_found,
                            fetch_attempt,
                            snapshot_generation,
                        );
                    } else {
                        fetch_snapshot(
                            snapshot_pump.clone(),
                            conversation_id,
                            not_found,
                            fetch_attempt,
                            snapshot_generation,
                        );
                    }
                }
            },
        ));
    }
    use_drop({
        let pump = pump.clone();
        let conversation_id = conversation_id.clone();
        move || {
            #[cfg(target_arch = "wasm32")]
            pump.dispatch(ClientEvent::AgentChat(AgentChatIntent::Forget {
                conversation_id,
            }));
            #[cfg(not(target_arch = "wasm32"))]
            let _ = (pump, conversation_id);
        }
    });
    if not_found() {
        return rsx! { EmptyState { icon: IconType::Bot, title: "This conversation no longer exists" } };
    }
    let Some(conversation) = conversation else {
        return rsx! { LoadingTranscript {} };
    };
    if conversation.is_native() {
        return rsx! {
            div { class: "flex min-h-0 flex-1 items-center justify-center p-6",
                role: "status",
                "data-testid": "archived-native-agent-notice",
                EmptyState {
                    icon: IconType::Bot,
                    title: "Archived Rust-native conversation",
                    supporting: "The custom Rust agent is archived. Its conversation data is retained. Start new conversations with OMP.",
                    action: None,
                }
            }
        };
    }
    if !transcript_loaded {
        return rsx! { div { class: ROOT_CLASS, LoadingTranscript {} } };
    }
    rsx! {
        OmpUiMount {
            key: "{conversation_id}",
            conversation,
            active,
            host_connected,
            pump,
        }
    }
}

#[component]
fn LoadingTranscript() -> Element {
    rsx! {
        div { class: TRANSCRIPT_CLASS, "aria-busy": "true",
            div { class: "mx-auto flex w-full max-w-reading flex-col gap-3",
                Skeleton { class: "ml-auto h-8 w-2/5" }
                Skeleton { class: "h-4 w-full" }
                Skeleton { class: "h-4 w-full" }
                Skeleton { class: "h-4 w-3/5" }
            }
        }
    }
}

type StoreReading = (
    Option<roost_protocol::wire::agent_chat::ConversationSummary>,
    bool,
    bool,
    u64,
    bool,
);

fn read_store(pump: &crate::pump::Pump, conversation_id: &str) -> StoreReading {
    let core = pump.core();
    let core = core.borrow();
    let store = core.store();
    let conversation = store.agent_chat.conversations.get(conversation_id).cloned();
    let loaded = store.agent_chat.transcripts.get(conversation_id);
    (
        conversation,
        loaded.is_some(),
        loaded.is_some_and(|loaded| loaded.stale),
        loaded.map(|loaded| loaded.seq).unwrap_or_default(),
        store.agent_chat.host_connected,
    )
}
