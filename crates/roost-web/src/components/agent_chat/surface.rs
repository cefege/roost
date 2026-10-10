//! The chat in an agent deck tab: header, then either the welcome (nothing said
//! yet, composer centred) or the transcript with the composer docked below.
//! It owns the snapshot lifecycle (fetch when absent or stale, retry, not
//! found), the model catalog, the draft, stick-to-bottom scrolling and the
//! code-block Copy delegation. Mounted by the deck's `AgentSlot`.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{AgentRunState, ModelsCatalog, Transcript};

use super::composer::AgentComposer;
use super::header::{AgentChatHeader, short_path};
use super::welcome::AgentWelcome;
use crate::components::md::{ButtonVariant, EmptyState, IconButton, IconButtonSize, Skeleton};
use crate::pump::use_store;

#[cfg(target_arch = "wasm32")]
use roost_client_core::ClientEvent;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::agent_chat::AgentChatIntent;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::ConnectCode;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::{GetAgentChatSnapshot, ListAgentModels};

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
    let _revision = pump.revision()();
    let (conversation, transcript, _stale, host_connected, online) =
        read_store(&pump, &conversation_id);
    let not_found = use_signal(|| false);
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_variables))]
    let fetch_attempt = use_signal(|| 0_u32);
    #[allow(unused_mut)]
    let mut catalog = use_signal(|| None::<ModelsCatalog>);
    let mut draft = use_signal(String::new);
    let mut field = use_signal(|| None::<std::rc::Rc<MountedData>>);
    let mut scroll_node = use_signal(|| None::<std::rc::Rc<MountedData>>);
    // Decided on scroll, not after render: by the time a large chunk of output
    // has rendered, the distance to the bottom already includes it.
    let mut pinned_to_bottom = use_signal(|| true);
    #[cfg(target_arch = "wasm32")]
    {
        let fetch_needed = transcript.is_none() || _stale;
        let snapshot_pump = pump.clone();
        use_effect(use_reactive(
            (&conversation_id, &fetch_needed, &fetch_attempt()),
            move |(conversation_id, fetch_needed, _attempt)| {
                if fetch_needed {
                    fetch_snapshot(
                        snapshot_pump.clone(),
                        conversation_id,
                        not_found,
                        fetch_attempt,
                    );
                }
            },
        ));
        let models_pump = pump.clone();
        use_effect(use_reactive((&conversation_id,), move |_| {
            let pump = models_pump.clone();
            wasm_bindgen_futures::spawn_local(async move {
                match pump.rpc().call(&ListAgentModels).await {
                    Ok(models) => catalog.set(Some(models)),
                    Err(error) => {
                        tracing::warn!(target: "agent_chat", %error, "agent models unavailable")
                    }
                }
            });
        }));
        let transcript_for_scroll = transcript.clone();
        use_effect(use_reactive((&transcript_for_scroll,), move |_| {
            if let (Some(node), true) = (scroll_node(), *pinned_to_bottom.peek()) {
                scroll_to_bottom(&node);
            }
        }));
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
        return rsx! { EmptyState { icon: "smart_toy", title: "This conversation no longer exists" } };
    }
    let Some(conversation) = conversation else {
        return rsx! { LoadingTranscript {} };
    };
    let header =
        rsx! { AgentChatHeader { conversation: conversation.clone(), online, pump: pump.clone() } };
    let Some(transcript) = transcript else {
        return rsx! { div { class: "agent-chat", {header} LoadingTranscript {} } };
    };
    let running = transcript.run_state == AgentRunState::Running;
    let composer = rsx! {
        AgentComposer {
            conversation: conversation.clone(),
            catalog: catalog(),
            draft,
            running,
            host_connected,
            pump: pump.clone(),
            on_field_mounted: move |node| field.set(Some(node)),
        }
    };
    if is_blank(&transcript) {
        return rsx! {
            div { class: "agent-chat", "data-empty": "true",
                {header}
                AgentWelcome {
                    folder: short_path(&conversation.cwd).rsplit('/').next().unwrap_or_default().to_string(),
                    machine: conversation.worker_label.clone(),
                    on_suggest: move |text: String| {
                        draft.set(text);
                        focus(field());
                    },
                    {composer}
                }
            }
        };
    }
    rsx! {
        div { class: "agent-chat",
            {header}
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
                },
                onclick: move |event: MouseEvent| copy_code_block(&event),
                div { class: "agent-chat__column",
                    super::transcript::AgentTranscriptView { transcript }
                }
            }
            div { class: "agent-chat__dock",
                if !pinned_to_bottom() {
                    IconButton {
                        icon: "arrow_downward",
                        label: "Jump to the latest message",
                        title: "Jump to latest",
                        variant: ButtonVariant::Outline,
                        size: IconButtonSize::IconSm,
                        class: "agent-chat__jump",
                        onclick: move |_| {
                            pinned_to_bottom.set(true);
                            #[cfg(target_arch = "wasm32")]
                            if let Some(node) = scroll_node() {
                                scroll_to_bottom(&node);
                            }
                        },
                    }
                }
                {composer}
            }
        }
    }
}

#[component]
fn LoadingTranscript() -> Element {
    rsx! {
        div { class: "agent-chat__transcript", "aria-busy": "true",
            div { class: "agent-chat__column agent-chat__loading",
                Skeleton { class: Some("agent-chat__loading-user".to_string()) }
                Skeleton { class: Some("agent-chat__loading-line".to_string()) }
                Skeleton { class: Some("agent-chat__loading-line".to_string()) }
                Skeleton { class: Some("agent-chat__loading-short".to_string()) }
            }
        }
    }
}

type StoreReading = (
    Option<roost_protocol::wire::agent_chat::ConversationSummary>,
    Option<Transcript>,
    bool,
    bool,
    bool,
);

fn read_store(pump: &crate::pump::Pump, conversation_id: &str) -> StoreReading {
    let core = pump.core();
    let core = core.borrow();
    let store = core.store();
    let conversation = store.agent_chat.conversations.get(conversation_id).cloned();
    let online = conversation.as_ref().is_some_and(|conversation| {
        store
            .workers
            .get(&conversation.worker_fp)
            .is_some_and(|worker| {
                roost_client_core::store::navigation::worker_online(
                    worker,
                    store.routable_worker_fps.as_ref(),
                    i64::try_from(core.clock().now_ms()).unwrap_or(i64::MAX),
                )
            })
    });
    let loaded = store.agent_chat.transcripts.get(conversation_id);
    (
        conversation,
        loaded.map(|loaded| loaded.transcript.clone()),
        loaded.is_some_and(|loaded| loaded.stale),
        store.agent_chat.host_connected,
        online,
    )
}

/// Nothing said and nothing running: the welcome, not an empty log.
fn is_blank(transcript: &Transcript) -> bool {
    transcript.items.is_empty() && transcript.run_state != AgentRunState::Running
}

#[cfg_attr(not(target_arch = "wasm32"), allow(unused_variables))]
fn focus(field: Option<std::rc::Rc<MountedData>>) {
    #[cfg(target_arch = "wasm32")]
    if let Some(field) = field {
        wasm_bindgen_futures::spawn_local(async move {
            let _ = field.set_focus(true).await;
        });
    }
}

#[cfg(target_arch = "wasm32")]
fn fetch_snapshot(
    pump: crate::pump::Pump,
    conversation_id: String,
    mut not_found: Signal<bool>,
    mut fetch_attempt: Signal<u32>,
) {
    not_found.set(false);
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
}

/// A click on a code block's Copy button copies that block and says so.
#[cfg_attr(not(target_arch = "wasm32"), allow(unused_variables))]
fn copy_code_block(event: &MouseEvent) {
    #[cfg(target_arch = "wasm32")]
    {
        use dioxus::web::WebEventExt as _;
        use wasm_bindgen::JsCast as _;

        let Some(button) = event
            .as_web_event()
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
            .and_then(|element| {
                element
                    .closest(&format!("[{}]", super::markdown::COPY_CODE_ATTRIBUTE))
                    .ok()
                    .flatten()
            })
        else {
            return;
        };
        let code = button
            .closest(".agent-chat__code")
            .ok()
            .flatten()
            .and_then(|block| block.query_selector("code").ok().flatten())
            .and_then(|code| code.text_content())
            .unwrap_or_default();
        if crate::components::notifications::clipboard::copy_text(&code) {
            button.set_text_content(Some("Copied"));
            wasm_bindgen_futures::spawn_local(async move {
                crate::components::terminal::dom::sleep_ms(1_500).await;
                button.set_text_content(Some("Copy"));
            });
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
