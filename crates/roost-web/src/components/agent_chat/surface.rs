//! The native agent workspace: inspector, transcript and composer inside a
//! deck agent tab. Owns snapshot repair, catalog loading, draft and reader
//! position; `workspace` owns responsive inspector placement. Mounted by
//! `AgentSlot`, with all conversation state supplied by the client store.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{AgentRunState, ModelsCatalog, ProviderEntry, Transcript};

use super::composer::AgentComposer;
use super::context_usage::{current_context_window, header_usage};
use super::draft::use_conversation_draft;
use super::header::AgentChatHeader;
use super::loop_controls::LoopControls;
use super::model_controls::{ModelSetup, ModelSetupCard};
use super::model_hub::ModelHub;
use super::queued_band::QueuedBand;
#[cfg(target_arch = "wasm32")]
use super::surface_dom::{distance_from_bottom, scroll_to_bottom};
use super::surface_state::{assistant_is_streaming, latest_todo, loop_status};
use super::todo_card::TodoCard;
use super::working_row::{WorkingRow, working_label};
use super::workspace::AgentWorkspace;
use crate::pump::use_store;
use libero::components::{ActionIcon, Box, Flex, Pictogram, Skeleton, Text};
use libero::sx::sx;

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
    let root_sx = use_hook(|| {
        sx().position("relative")
            .display("flex")
            .flex_direction("column")
            .height("100%")
            .width("100%")
            .min_height("0")
            .min_width("0")
            .background("surface")
            .color("ink")
    });
    let transcript_sx = use_hook(|| {
        sx().min_height("0")
            .flex("1")
            .overflow_y("auto")
            .with("overscroll-behavior-y", "contain")
            .padding_inline("lg")
            .padding_block("xl")
    });
    let column_sx = use_hook(|| sx().margin_inline("auto").width("100%").max_width("48rem"));
    let rail_sx = use_hook(|| {
        sx().position("relative")
            .margin_inline("auto")
            .display("flex")
            .width("100%")
            .max_width("48rem")
            .flex_shrink("0")
            .flex_direction("column")
            .gap("md")
            .padding_inline("lg")
            .padding_top("sm")
            .padding_bottom("md")
    });
    let jump_sx = use_hook(|| {
        sx().position("absolute")
            .bottom("100%")
            .left("50%")
            .z_index("10")
            .margin_bottom("sm")
            .with("transform", "translateX(-50%)")
            .box_shadow("md")
    });
    let pump = use_store();
    let _revision = pump.revision()();
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_variables))]
    let (conversation, transcript, _stale, loaded_seq, host_connected, online) =
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
        return rsx! {
            Flex { direction: "column", sx: root_sx, role: "status",
                Pictogram { icon: pictogram_icons_lucide::bot::outlined, aria_label: "Assistant" }
                Text { "This conversation no longer exists" }
            }
        };
    }
    let Some(conversation) = conversation else {
        return rsx! { LoadingTranscript {} };
    };
    let usage = transcript.as_ref().and_then(|transcript| {
        let window = current_context_window(catalog.read().as_ref(), &conversation);
        header_usage(
            transcript.usage.context_tokens,
            transcript.usage.input_tokens + transcript.usage.output_tokens,
            transcript.usage.cost_usd,
            window,
        )
    });
    let header = rsx! {
        AgentChatHeader { conversation: conversation.clone(), online, usage, pump: pump.clone() }
    };
    let Some(transcript) = transcript else {
        return rsx! {
            Flex { direction: "column", sx: root_sx, {header} LoadingTranscript {} }
        };
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
            header,
            Box {
                component: "div",
                sx: transcript_sx,
                role: "log",
                "aria-live": "polite",
                "aria-relevant": "additions text",
                "aria-label": "Conversation transcript",
                onmounted: move |event: MountedEvent| scroll_node.set(Some(event.data())),
                onscroll: move |_| {
                    #[cfg(target_arch = "wasm32")]
                    if let Some(node) = scroll_node() {
                        let near = distance_from_bottom(&node).is_some_and(|distance| distance < STICK_THRESHOLD_PX);
                        if near != *pinned_to_bottom.peek() {
                            pinned_to_bottom.set(near);
                        }
                    }
                },
                Flex { direction: "column", sx: column_sx,
                    if transcript.items.is_empty() && !running {
                        super::welcome::AgentWelcome { conversation_id: conversation_id.clone(), draft, connected: host_connected }
                    }
                    super::transcript::AgentTranscriptView { transcript: transcript.clone(), conversation_id: conversation_id.clone(), pump: pump.clone(), draft }
                }
            }
            Flex { direction: "column", sx: rail_sx,
                if running {
                    WorkingRow {
                        label: if assistant_is_streaming(&transcript) { "Writing…".to_string() } else { working_label(&transcript.items) },
                    }
                }
                if !pinned_to_bottom() {
                    ActionIcon {
                        icon: pictogram_icons_lucide::arrow_down::outlined,
                        aria_label: "Jump to the latest message",
                        title: "Jump to latest",
                        sx: jump_sx,
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
                ModelSetupCard { setup, on_retry: retry_catalog }
                {composer}
            }
        }
    }
}

#[component]
fn LoadingTranscript() -> Element {
    let transcript_sx = use_hook(|| {
        sx().min_height("0")
            .flex("1")
            .overflow_y("auto")
            .with("overscroll-behavior-y", "contain")
            .padding_inline("lg")
            .padding_block("xl")
    });
    let column_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .gap("md")
            .margin_inline("auto")
            .width("100%")
            .max_width("48rem")
    });
    rsx! {
        Box { component: "div", sx: transcript_sx, "aria-busy": "true",
            Flex { direction: "column", sx: column_sx,
                Skeleton { width: "40%", height: "2rem" }
                Skeleton { width: "100%", height: "1rem" }
                Skeleton { width: "100%", height: "1rem" }
                Skeleton { width: "60%", height: "1rem" }
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
        loaded.map(|loaded| loaded.seq).unwrap_or_default(),
        store.agent_chat.host_connected,
        online,
    )
}
