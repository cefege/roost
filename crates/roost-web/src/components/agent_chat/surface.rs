//! The route-owned chat view: snapshot lifecycle, transcript, and composer.
//! It reads the client replica and fetches a fresh snapshot whenever replay has
//! made the loaded transcript stale. The RPC call itself is wasm-only.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::Transcript;

use crate::components::md::{EmptyState, Surface};
use crate::pump::use_store;
use crate::router_state::use_navigate;

#[cfg(target_arch = "wasm32")]
use roost_client_core::ClientEvent;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::agent_chat::AgentChatIntent;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::ConnectCode;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::GetAgentChatSnapshot;

/// How long a failed snapshot fetch waits before it is tried again.
#[cfg(target_arch = "wasm32")]
const SNAPSHOT_RETRY_MS: u64 = 2_000;

/// Scrolled further than this from the bottom, the reader is reading history
/// and new output must not pull the view down.
#[cfg(target_arch = "wasm32")]
const STICK_THRESHOLD_PX: i32 = 80;

#[component]
pub fn AgentChatSurface(conversation_id: String) -> Element {
    let pump = use_store();
    let navigate = use_navigate();
    let _revision = pump.revision()();
    let (conversation, transcript, _stale, host_connected) = {
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        let conversation = store
            .agent_chat
            .conversations
            .get(&conversation_id)
            .cloned();
        let loaded = store.agent_chat.transcripts.get(&conversation_id);
        (
            conversation,
            loaded.map(|loaded| loaded.transcript.clone()),
            loaded.is_some_and(|loaded| loaded.stale),
            store.agent_chat.host_connected,
        )
    };
    #[cfg(target_arch = "wasm32")]
    let fetch_needed = transcript.is_none() || _stale;
    #[allow(unused_mut)]
    let mut not_found = use_signal(|| false);
    #[allow(unused_mut)]
    let mut fetch_attempt = use_signal(|| 0_u32);
    #[cfg(target_arch = "wasm32")]
    let snapshot_pump = pump.clone();
    #[cfg(target_arch = "wasm32")]
    use_effect(use_reactive(
        (&conversation_id, &fetch_needed, &fetch_attempt()),
        move |(conversation_id, fetch_needed, _attempt)| {
            if !fetch_needed {
                return;
            }
            not_found.set(false);
            let pump = snapshot_pump.clone();
            let mut not_found = not_found;
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
                            seq,
                            transcript,
                        }))
                    }
                    // The surface may have unmounted while the call was in flight.
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
        },
    ));
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (&not_found, &fetch_attempt);
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
    let mut scroll_node = use_signal(|| None::<std::rc::Rc<MountedData>>);
    // Decided on scroll, not after render: by the time a large chunk of output
    // has rendered, the distance to the bottom already includes it.
    #[allow(unused_mut)]
    let mut pinned_to_bottom = use_signal(|| true);
    #[cfg(target_arch = "wasm32")]
    let transcript_for_scroll = transcript.clone();
    #[cfg(target_arch = "wasm32")]
    use_effect(use_reactive((&transcript_for_scroll,), move |_| {
        if let (Some(node), true) = (scroll_node(), *pinned_to_bottom.peek()) {
            scroll_to_bottom(&node);
        }
    }));
    if not_found() {
        return rsx! { EmptyState { icon: "smart_toy", title: "Conversation not found" } };
    }
    if conversation.is_none() && transcript.is_none() {
        return rsx! { EmptyState { icon: "smart_toy", title: "Loading conversation" } };
    }
    let Some(conversation) = conversation else {
        return rsx! { EmptyState { icon: "smart_toy", title: "Loading conversation" } };
    };
    let transcript = transcript.unwrap_or_else(|| Transcript {
        items: Vec::new(),
        run_state: roost_protocol::wire::agent_chat::AgentRunState::Idle,
        error: None,
        model: None,
        thinking_level: None,
        usage: Default::default(),
    });
    rsx! {
        div { class: "agent-chat",
            super::header::AgentChatHeader { conversation: conversation.clone(), pump: pump.clone(), navigate }
            div {
                class: "agent-chat__transcript",
                role: "log",
                "aria-live": "polite",
                "aria-relevant": "additions text",
                "aria-label": "Conversation transcript",
                onmounted: move |event| scroll_node.set(Some(event.data())),
                onscroll: move |_| {
                    #[cfg(target_arch = "wasm32")]
                    if let Some(node) = scroll_node() {
                        let near = distance_from_bottom(&node).is_some_and(|distance| distance < STICK_THRESHOLD_PX);
                        if near != *pinned_to_bottom.peek() {
                            pinned_to_bottom.set(near);
                        }
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    let _ = &mut pinned_to_bottom;
                },
                if transcript.items.is_empty() && transcript.run_state == roost_protocol::wire::agent_chat::AgentRunState::Idle {
                    Surface { class: Some("agent-chat__empty".to_string()), "Start a conversation by sending a message." }
                } else {
                    super::transcript::AgentTranscriptView { transcript: transcript.clone() }
                }
            }
            super::composer::AgentComposer {
                conversation_id: conversation_id.clone(),
                running: transcript.run_state == roost_protocol::wire::agent_chat::AgentRunState::Running,
                host_connected,
                pump: pump.clone(),
            }
        }
    }
}
#[cfg(target_arch = "wasm32")]
fn transcript_element(node: &MountedData) -> Option<web_sys::HtmlElement> {
    use dioxus::web::WebEventExt as _;
    use wasm_bindgen::JsCast as _;

    node.try_as_web_event()
        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
}

#[cfg(target_arch = "wasm32")]
fn distance_from_bottom(node: &MountedData) -> Option<i32> {
    let element = transcript_element(node)?;
    Some(element.scroll_height() - element.scroll_top() - element.client_height())
}

#[cfg(target_arch = "wasm32")]
fn scroll_to_bottom(node: &MountedData) {
    if let Some(element) = transcript_element(node) {
        element.set_scroll_top(element.scroll_height());
    }
}
