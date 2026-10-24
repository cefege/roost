//! Proposed plans are actionable cards; decisions are persisted by the harness.
//! The transcript supplies plan identity and state from the canonical fold.
//! Approved plans either continue here or navigate to a newly created chat.

use crate::components::ui::Markdown;
use crate::pump::{Pump, use_store};
use crate::router_state::use_navigate;
use dioxus::prelude::*;
use libero::{
    components::{Button, Flex, Paper, Textarea, Title},
    sx::sx,
};

#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::DecideAgentPlan;

#[component]
pub fn PlanCard(
    id: String,
    title: String,
    content: String,
    state: String,
    conversation_id: String,
) -> Element {
    let mut refine = use_signal(|| false);
    let mut feedback = use_signal(String::new);
    let pump = use_store();
    let navigate = use_navigate();
    let proposed = state == "proposed";
    let approve_pump = pump.clone();
    let approve_id = conversation_id.clone();
    let approve_item = id.clone();
    let new_pump = pump.clone();
    let new_id = conversation_id.clone();
    let new_item = id.clone();
    let refine_pump = pump;
    let refine_id = conversation_id;
    let refine_item = id.clone();
    let open_refine = move |_| refine.set(true);
    let card = use_hook(|| sx().gap("sm").padding("sm").width("100%"));
    let actions = use_hook(|| sx().gap("sm"));
    let refinement = use_hook(|| sx().width("100%"));
    rsx! {
        Paper { key: "{id}", sx: card,
            Flex { direction: "column", gap: "sm",
                Title { size: "sm", "{title}" }
                Markdown { text: content }
                if proposed {
                    Flex { sx: actions,
                        Button { variant: "filled", color: "primary",
                            onclick: move |_| decide(approve_pump.clone(), approve_id.clone(), approve_item.clone(), "approve", String::new()),
                            "Approve"
                        }
                        Button { variant: "tonal", color: "secondary",
                            onclick: move |_| {
                                approve_new(new_pump.clone(), new_id.clone(), new_item.clone(), navigate);
                            },
                            "Approve in new chat"
                        }
                        Button { variant: "standard", color: "primary", onclick: open_refine, "Refine…" }
                    }
                    if refine() {
                        Flex { direction: "column", align: "start", gap: "sm",
                            Textarea {
                                sx: refinement,
                                value: feedback(),
                                rows: 3,
                                label: "Plan refinement",
                                oninput: move |value: String| feedback.set(value),
                            }
                            Button {
                                variant: "filled",
                                color: "primary",
                                onclick: move |_| {
                                    decide(refine_pump.clone(), refine_id.clone(), refine_item.clone(), "refine", feedback());
                                    refine.set(false);
                                },
                                "Send refinement"
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn decide(
    pump: Pump,
    conversation_id: String,
    item_id: String,
    decision: &'static str,
    feedback: String,
) {
    wasm_bindgen_futures::spawn_local(async move {
        if let Err(error) = pump
            .rpc()
            .call(&DecideAgentPlan {
                conversation_id,
                item_id,
                decision: decision.to_string(),
                feedback,
                request_id: super::request_ids::new_request_id(),
            })
            .await
        {
            tracing::warn!(target: "agent_chat", %error, "plan decision failed");
        }
    });
}

#[cfg(not(target_arch = "wasm32"))]
fn decide(_: Pump, _: String, _: String, _: &'static str, _: String) {}

#[cfg(target_arch = "wasm32")]
fn approve_new(
    pump: Pump,
    conversation_id: String,
    item_id: String,
    navigate: EventHandler<String>,
) {
    wasm_bindgen_futures::spawn_local(async move {
        match pump
            .rpc()
            .call(&DecideAgentPlan {
                conversation_id,
                item_id,
                decision: "approve_new".to_string(),
                feedback: String::new(),
                request_id: super::request_ids::new_request_id(),
            })
            .await
        {
            Ok(new_id) if !new_id.is_empty() => navigate.call(crate::routes::agent_href(&new_id)),
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(target: "agent_chat", %error, "new plan conversation failed")
            }
        }
    });
}

#[cfg(not(target_arch = "wasm32"))]
fn approve_new(_: Pump, _: String, _: String, _: EventHandler<String>) {}
