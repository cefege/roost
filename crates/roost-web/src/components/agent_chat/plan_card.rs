//! Plan review pairs a section outline with a scrollable document and actions.
//! The canonical transcript owns proposal state; approval starts a fresh
//! execution conversation, while refinement uses the main chat composer.

mod outline;

use std::rc::Rc;

use dioxus::prelude::*;

use super::markdown::markdown_to_safe_html;
use super::plan_refinement::PlanRefinement;
use crate::components::ui::{
    Button, ButtonSize, ButtonVariant, Card, CardContent, CardFooter, CardHeader, CardSize,
    CardTitle, DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger, Markdown,
};
use crate::pump::{Pump, use_store};
use crate::router_state::use_navigate;

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
    let pump = use_store();
    let navigate = use_navigate();
    let refinement = try_use_context::<PlanRefinement>();
    let mut selected = use_signal(|| 0usize);
    let pending = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let mut more_open = use_signal(|| false);
    let document = use_memo(use_reactive((&content,), |(content,)| {
        (
            markdown_to_safe_html(&content),
            Rc::<[outline::PlanSection]>::from(outline::sections(&content)),
        )
    }));
    let document_id = format!("plan-document-{id}");
    #[cfg(target_arch = "wasm32")]
    let select_document_id = document_id.clone();
    let proposed = state == "proposed";
    let approve_pump = pump.clone();
    let approve_conversation = conversation_id.clone();
    let approve_item = id.clone();
    let continue_pump = pump;
    let continue_conversation = conversation_id;
    let continue_item = id.clone();
    let refine_item = id.clone();
    let refine_title = title.clone();
    let view = document.read();
    rsx! {
        Card {
            key: "{id}",
            size: CardSize::Sm,
            class: "min-w-0",
            "data-testid": "agent-plan-review",
            "aria-busy": pending(),
            CardHeader { CardTitle { "{title}" } }
            CardContent {
                div { class: "flex min-h-0 min-w-0 max-h-96 gap-3 md:max-h-128",
                    if !view.1.is_empty() {
                        outline::PlanOutline {
                            sections: view.1.clone(),
                            selected: selected().min(view.1.len().saturating_sub(1)),
                            on_select: move |index| {
                                selected.set(index);
                                #[cfg(target_arch = "wasm32")]
                                outline::scroll_to_section(&select_document_id, index);
                            },
                        }
                    }
                    div {
                        id: document_id,
                        class: "min-h-0 min-w-0 flex-1 overflow-y-auto overscroll-y-contain",
                        "aria-label": "Plan document",
                        tabindex: "0",
                        Markdown { html: view.0.clone(), class: "font-mono" }
                    }
                }
            }
            CardFooter {
                div { class: "flex w-full flex-col gap-2 border-t border-border pt-3",
                    if proposed {
                        div { class: "flex flex-wrap items-center gap-2",
                            Button {
                                size: ButtonSize::Sm,
                                disabled: pending(),
                                onclick: move |_| submit_decision(
                                    approve_pump.clone(), approve_conversation.clone(),
                                    approve_item.clone(), "approve_new", navigate, pending, error,
                                ),
                                if pending() { "Starting…" } else { "Approve in new chat" }
                            }
                            Button {
                                variant: ButtonVariant::Secondary,
                                size: ButtonSize::Sm,
                                disabled: pending() || refinement.is_none(),
                                onclick: move |_| {
                                    if let Some(refinement) = refinement {
                                        match refinement.begin(refine_item.clone(), refine_title.clone()) {
                                            Ok(()) => error.set(None),
                                            Err(message) => error.set(Some(message.to_owned())),
                                        }
                                    }
                                },
                                "Refine"
                            }
                            DropdownMenu {
                                open: more_open(),
                                on_open_change: move |next| more_open.set(next),
                                DropdownMenuTrigger {
                                    ghost: true,
                                    disabled: pending(),
                                    aria_label: "Other plan actions",
                                    "More"
                                }
                                if more_open() {
                                    DropdownMenuContent {
                                        DropdownMenuItem {
                                            onclick: move |_| {
                                                more_open.set(false);
                                                submit_decision(
                                                    continue_pump.clone(), continue_conversation.clone(),
                                                    continue_item.clone(), "approve", navigate, pending, error,
                                                );
                                            },
                                            "Execute in this chat"
                                        }
                                    }
                                }
                            }
                        }
                    } else {
                        p { class: "text-xs text-muted-foreground",
                            match state.as_str() {
                                "approved" => "Plan approved",
                                "refined" => "Revision requested in this chat",
                                _ => "Plan is no longer awaiting approval",
                            }
                        }
                    }
                    if let Some(message) = error() {
                        p { role: "alert", class: "text-sm text-destructive", "{message}" }
                    }
                }
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn submit_decision(
    pump: Pump,
    conversation_id: String,
    item_id: String,
    decision: &'static str,
    navigate: EventHandler<String>,
    mut pending: Signal<bool>,
    mut error: Signal<Option<String>>,
) {
    if *pending.peek() {
        return;
    }
    pending.set(true);
    error.set(None);
    spawn(async move {
        let result = pump
            .rpc()
            .call(&DecideAgentPlan {
                conversation_id,
                item_id,
                decision: decision.to_owned(),
                feedback: String::new(),
                request_id: super::request_ids::new_request_id(),
            })
            .await;
        pending.set(false);
        match result {
            Ok(new_id) if decision == "approve_new" && !new_id.is_empty() => {
                navigate.call(crate::routes::agent_href(&new_id));
            }
            Ok(_) if decision == "approve_new" => {
                error.set(Some(
                    "Approval returned no execution chat. Reload to check the plan status."
                        .to_owned(),
                ));
            }
            Ok(_) => {}
            Err(failure) => {
                tracing::warn!(target: "agent_chat", %failure, decision, "plan decision failed");
                error.set(Some(failure.to_string()));
            }
        }
    });
}

#[cfg(not(target_arch = "wasm32"))]
fn submit_decision(
    _: Pump,
    _: String,
    _: String,
    _: &'static str,
    _: EventHandler<String>,
    _: Signal<bool>,
    _: Signal<Option<String>>,
) {
}
