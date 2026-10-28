//! The native agent workspace: inspector, transcript and composer inside a
//! deck agent tab. Owns snapshot repair, catalog loading, draft and reader
//! position; `workspace` owns responsive inspector placement. Mounted by
//! `AgentSlot`, with all conversation state supplied by the client store.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{AgentRunState, ModelsCatalog, ProviderEntry, Transcript};

use super::composer::AgentComposer;
use super::draft::use_conversation_draft;
use super::interaction_cards::TodoCard;
use super::last_conversation::use_remember_conversation;
use super::loop_controls::LoopControls;
use super::message_actions::EditTarget;
use super::model_controls::{ModelSetup, ModelSetupNotice};
use super::model_hub::ModelHub;
use super::queued_band::QueuedBand;
use super::surface_dom::copy_code_block;
#[cfg(target_arch = "wasm32")]
use super::surface_dom::{distance_from_bottom, scroll_to_bottom};
use super::surface_state::{assistant_is_streaming, latest_todo, loop_status};
use super::working_row::{WorkingRow, working_label};
use super::workspace::AgentWorkspace;
use crate::components::ui::{
    ButtonSize, ButtonVariant, EmptyState, IconButton, IconType, Skeleton,
};
use crate::pump::use_store;

const ROOT_CLASS: &str =
    "relative flex h-full w-full min-h-0 min-w-0 flex-col bg-background text-foreground";
const TRANSCRIPT_CLASS: &str = "min-h-0 flex-1 overflow-y-auto overscroll-y-contain py-5";
const COLUMN_CLASS: &str = "mx-auto w-full max-w-reading px-4";

#[cfg(target_arch = "wasm32")]
use super::surface_fetch::{fetch_events_or_snapshot, fetch_snapshot};
#[cfg(target_arch = "wasm32")]
use roost_client_core::ClientEvent;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::agent_chat::AgentChatIntent;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::ListAgentModels;

/// Scrolled further than this from the bottom, the reader is reading history
/// and new output must not pull the view down.
#[cfg(target_arch = "wasm32")]
const STICK_THRESHOLD_PX: i32 = 80;

#[component]
pub fn AgentChatSurface(conversation_id: String, active: bool) -> Element {
    let pump = use_store();
    let _revision = pump.revision()();
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_variables))]
    let (conversation, transcript, _stale, loaded_seq, host_connected) =
        read_store(&pump, &conversation_id);
    let not_found = use_signal(|| false);
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_variables))]
    let fetch_attempt = use_signal(|| 0_u32);
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_variables))]
    let snapshot_generation = use_signal(|| 0_u64);
    #[allow(unused_mut)]
    let mut catalog = use_signal(|| None::<ModelsCatalog>);
    #[allow(unused_mut)]
    let mut catalog_failed = use_signal(|| false);
    let mut catalog_attempt = use_signal(|| 0_u32);
    let draft = use_conversation_draft(&conversation_id);
    use_remember_conversation(&conversation_id, active);
    let mut edit_target = use_signal(|| None::<EditTarget>);
    let mut edit_target_owner = use_signal(|| conversation_id.clone());
    if *edit_target_owner.peek() != conversation_id {
        edit_target.set(None);
        edit_target_owner.set(conversation_id.clone());
    }
    let model_hub_open = use_signal(|| false);
    let connect_provider = use_signal(|| None::<ProviderEntry>);
    // Opening the hub refetches the catalog so its recent models are current.
    use_effect(move || {
        if model_hub_open() {
            let next = *catalog_attempt.peek() + 1;
            catalog_attempt.set(next);
        }
    });
    let mut scroll_node = use_signal(|| None::<std::rc::Rc<MountedData>>);
    // Decided on scroll, not after render: by the time a large chunk of output
    // has rendered, the distance to the bottom already includes it.
    let mut pinned_to_bottom = use_signal(|| true);
    #[cfg(target_arch = "wasm32")]
    {
        let fetch_needed = transcript.is_none() || _stale;
        let has_transcript = transcript.is_some();
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
                    if has_transcript {
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
        let models_pump = pump.clone();
        use_effect(use_reactive(
            (&conversation_id, &catalog_attempt()),
            move |_| {
                let pump = models_pump.clone();
                catalog_failed.set(false);
                wasm_bindgen_futures::spawn_local(async move {
                    match pump.rpc().call(&ListAgentModels).await {
                        Ok(models) => catalog.set(Some(models)),
                        Err(error) => {
                            tracing::warn!(target: "agent_chat", %error, "agent models unavailable");
                            if let Ok(mut failed) = catalog_failed.try_write() {
                                *failed = true;
                            }
                        }
                    }
                });
            },
        ));
        let transcript_for_scroll = transcript.clone();
        use_effect(use_reactive((&transcript_for_scroll,), move |_| {
            if let (Some(node), true) = (scroll_node(), *pinned_to_bottom.peek()) {
                scroll_to_bottom(&node);
            }
        }));
        // The draft sizes the composer's field and so the transcript above it;
        // re-pin so the newest messages move up instead of hiding. Read here,
        // not in the body, so a keystroke reruns this effect, not the surface.
        use_effect(move || {
            let _ = draft.read();
            if let (Some(node), true) = (scroll_node(), *pinned_to_bottom.peek()) {
                scroll_to_bottom(&node);
            }
        });
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
    let Some(transcript) = transcript else {
        return rsx! { div { class: ROOT_CLASS, LoadingTranscript {} } };
    };
    let running = transcript.run_state == AgentRunState::Running;
    let setup = ModelSetup::from_catalog(catalog.read().as_ref(), catalog_failed());
    let retry_catalog = move |()| catalog_attempt += 1;
    let composer = rsx! {
        if let Some(status) = loop_status(&transcript) {
            LoopControls { status: status.to_owned(), conversation_id: conversation_id.clone(), pump: pump.clone() }
        }
        AgentComposer {
            conversation: conversation.clone(),
            catalog: catalog(),
            catalog_failed: catalog_failed(),
            transcript: transcript.clone(),
            draft,
            edit_target,
            running,
            host_connected,
            active,
            pump: pump.clone(),
            model_hub_open,
            connect_provider,
        }
        ModelHub {
            open: model_hub_open,
            conversation: conversation.clone(),
            catalog: catalog(),
            pump: pump.clone(),
            connect_provider,
        }
    };
    rsx! {
        AgentWorkspace {
            conversation: conversation.clone(),
            transcript: transcript.clone(),
            catalog: catalog(),
            pump: pump.clone(),
            active,
            div { class: "flex min-h-0 flex-1",
                div { class: "flex min-h-0 min-w-0 flex-1 flex-col",
                    div {
                        class: TRANSCRIPT_CLASS,
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
                        div { class: COLUMN_CLASS,
                            if transcript.items.is_empty() && !running {
                                super::welcome::AgentWelcome {}
                            }
                            super::transcript::AgentTranscriptView { transcript: transcript.clone(), conversation_id: conversation_id.clone(), pump: pump.clone(), draft, edit_target }
                        }
                    }
                    div { class: "relative mx-auto flex w-full max-w-reading shrink-0 flex-col gap-3 pl-[max(env(safe-area-inset-left,0px),0.5rem)] pr-[max(env(safe-area-inset-right,0px),0.5rem)] pt-2 pb-(--term-chat-dock-rest-offset)",
                        if running {
                            WorkingRow {
                                label: if assistant_is_streaming(&transcript) { "Writing…".to_string() } else { working_label(&transcript.items) },
                            }
                        }
                        if !pinned_to_bottom() {
                            IconButton {
                                icon: IconType::ArrowDown,
                                label: "Jump to the latest message",
                                title: "Jump to latest",
                                variant: ButtonVariant::Secondary,
                                size: ButtonSize::IconSm,
                                class: "absolute bottom-full left-1/2 z-10 mb-2 -translate-x-1/2",
                                onclick: move |_| {
                                    pinned_to_bottom.set(true);
                                    #[cfg(target_arch = "wasm32")]
                                    if let Some(node) = scroll_node() {
                                        scroll_to_bottom(&node);
                                    }
                                },
                            }
                        }
                        if let Some(phases) = latest_todo(&transcript) { TodoCard { phases } }
                        QueuedBand { messages: transcript.queued.clone(), conversation_id: conversation_id.clone(), pump: pump.clone() }
                        ModelSetupNotice { setup, on_retry: retry_catalog }
                        {composer}
                    }
                }
            }
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
    Option<Transcript>,
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
        loaded.map(|loaded| loaded.transcript.clone()),
        loaded.is_some_and(|loaded| loaded.stale),
        loaded.map(|loaded| loaded.seq).unwrap_or_default(),
        store.agent_chat.host_connected,
    )
}
