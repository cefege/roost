//! The chat header traces the machine, working directory and conversation
//! lineage. It owns conversation picking, renaming and the context usage ring;
//! deletion stays behind its overflow menu. Mounted by `agent_chat::surface`.

use dioxus::prelude::*;
use libero::components::{
    ActionIcon, Button, CircularProgress, Dialog, Flex, Pictogram, Text, TextField,
};
use libero::hooks::{ModalScope, use_modal};
use roost_protocol::wire::agent_chat::ConversationSummary;
use std::{cell::RefCell, rc::Rc};

use super::context_usage::HeaderUsage;
use super::toolbar_menu::{ToolbarMenu, ToolbarMenuItem, ToolbarTrigger};
use crate::components::overlay_generation::OverlayGeneration;
use crate::pump::{Pump, use_store};
use crate::router_state::use_navigate;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::ConfigureAgentChat;

#[path = "header_lineage.rs"]
mod lineage;

use lineage::{conversation_lineage, short_path};
#[component]
pub fn AgentChatHeader(
    conversation: ConversationSummary,
    online: bool,
    usage: Option<HeaderUsage>,
    pump: Pump,
) -> Element {
    let store = use_store();
    let _revision = store.revision()();
    let mut conversations = {
        let core = store.core();
        let core = core.borrow();
        core.store()
            .agent_chat
            .conversations
            .values()
            .cloned()
            .collect::<Vec<_>>()
    };
    conversations.sort_by(|left, right| {
        (&left.worker_label, &left.cwd, &left.title).cmp(&(
            &right.worker_label,
            &right.cwd,
            &right.title,
        ))
    });
    let mut confirm_delete = use_signal(|| false);
    let mut picker_open = use_signal(|| false);
    let mut rename_open = use_signal(|| false);
    let mut rename_title = use_signal(|| conversation.title.clone());
    let mut display_title = use_signal(|| conversation.title.clone());
    let mut rename_pending = use_signal(|| false);
    let mut rename_error = use_signal(|| None::<String>);
    use_effect(use_reactive((&conversation.title,), move |(title,)| {
        display_title.set(title.clone())
    }));
    let conversation_id = conversation.id.clone();
    let lineage = conversation_lineage(&conversation, &conversations);
    let lineage_len = lineage.len();
    let rename_pump = pump.clone();
    let rename_conversation_id = conversation_id.clone();
    let navigate = use_navigate();
    let picker_generation = use_hook(|| Rc::new(RefCell::new(OverlayGeneration::default())));
    let rename_generation = use_hook(|| Rc::new(RefCell::new(OverlayGeneration::default())));
    let delete_generation = use_hook(|| Rc::new(RefCell::new(OverlayGeneration::default())));
    let picker_choices = conversations.clone();
    let picker_conversation_id = conversation_id.clone();
    let picker_display_title = display_title;
    let picker_modal = use_modal(move |scope: ModalScope<(), String>| {
        rsx! {
            Dialog { title: "Conversations", "aria-describedby": "agent-chat-conversations-description",
                Flex { direction: "column", gap: "sm",
                    Text { id: "agent-chat-conversations-description", size: "sm", color: "muted", "Choose an agent conversation." }
                    for choice in picker_choices.iter() {
                        {
                            let headline = if choice.id == picker_conversation_id {
                                super::display_conversation_title(&picker_display_title.read()).to_owned()
                            } else {
                                super::display_conversation_title(&choice.title).to_owned()
                            };
                            let choice_href = crate::routes::agent_href(&choice.id);
                            let support = format!(
                                "{} · {} · {}",
                                choice.worker_label,
                                short_path(&choice.cwd),
                                conversation_lineage(choice, &picker_choices)
                                    .iter()
                                    .map(|entry| if entry.id == picker_conversation_id {
                                        super::display_conversation_title(&picker_display_title.read()).to_owned()
                                    } else {
                                        super::display_conversation_title(&entry.title).to_owned()
                                    })
                                    .collect::<Vec<_>>()
                                    .join(" / "),
                            );
                            rsx! {
                                Button { variant: "standard", full_width: true, onclick: move |_| scope.resolve(choice_href.clone()),
                                    Flex { direction: "column", gap: "xs",
                                        Text { size: "sm", "{headline}" }
                                        Text { size: "xs", color: "muted", "{support}" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    });
    let rename_generation_for_modal = Rc::clone(&rename_generation);
    let rename_modal = use_modal(move |scope: ModalScope<u64, String>| {
        let Some(opening_generation) = scope.try_args() else {
            return rsx! {};
        };
        let modal_generation = Rc::clone(&rename_generation_for_modal);
        let modal_pump = rename_pump.clone();
        let modal_conversation_id = rename_conversation_id.clone();
        let modal_display_title = display_title;
        rsx! {
            Dialog { title: "Rename conversation", "aria-describedby": "agent-chat-rename-description",
                ondismiss: Callback::new(move |_| !rename_pending()),
                Flex { direction: "column", gap: "md",
                    Text { id: "agent-chat-rename-description", size: "sm", color: "muted", "Choose a name shown in the conversation picker." }
                    TextField {
                        value: rename_title(),
                        oninput: move |value: String| rename_title.set(value),
                        label: "Conversation name",
                        disabled: rename_pending(),
                        id: "agent-chat-rename-title",
                        "data-testid": "agent-chat-rename-title",
                    }
                    if let Some(error) = rename_error() {
                        Text { size: "sm", color: "error", role: "alert", "{error}" }
                    }
                    Flex { direction: "row", gap: "sm", justify: "end",
                        Button {
                            variant: "outlined",
                            disabled: rename_pending(),
                            onclick: move |_| scope.close(),
                            "Cancel"
                        }
                        Button {
                            disabled: rename_pending() || rename_title().trim().is_empty(),
                            "data-testid": "agent-chat-rename-save",
                            onclick: move |_| {
                                let title = rename_title.peek().trim().to_owned();
                                if title.is_empty() || rename_pending() { return; }
                                rename_pending.set(true);
                                rename_conversation_title(
                                    modal_pump.clone(),
                                    modal_conversation_id.clone(),
                                    title,
                                    opening_generation,
                                    Rc::clone(&modal_generation),
                                    rename_pending,
                                    rename_error,
                                    modal_display_title,
                                    scope,
                                );
                            },
                            if rename_pending() { "Saving…" } else { "Save name" }
                        }
                    }
                }
            }
        }
    });

    let delete_modal = use_modal(|scope: ModalScope<(), bool>| {
        rsx! {
            Dialog { title: "Delete conversation?", "aria-describedby": "agent-chat-delete-description",
                Text { id: "agent-chat-delete-description", size: "sm", color: "muted",
                    "The transcript is removed from Roost. You can undo for a few seconds after it closes."
                }
                Flex { direction: "row", gap: "sm", justify: "end",
                    Button { variant: "outlined", onclick: move |_| scope.close(), "Cancel" }
                    Button { variant: "outlined", color: "error", "data-testid": "agent-chat-delete-confirm",
                        onclick: move |_| scope.resolve(true), "Delete" }
                }
            }
        }
    });
    let header_sx = use_hook(|| {
        libero::sx::sx()
            .display("flex")
            .align_items("center")
            .flex_wrap("wrap")
            .gap("sm")
    });
    let nav_sx = use_hook(|| {
        libero::sx::sx()
            .display("flex")
            .align_items("center")
            .flex_wrap("wrap")
            .gap("xs")
            .flex("1")
            .min_width("0")
    });
    let controls_sx = use_hook(|| {
        libero::sx::sx()
            .display("flex")
            .align_items("center")
            .gap("xs")
    });
    let teardown_picker = Rc::clone(&picker_generation);
    let teardown_rename = Rc::clone(&rename_generation);
    let teardown_delete = Rc::clone(&delete_generation);
    use_drop(move || {
        teardown_picker.borrow_mut().invalidate();
        teardown_rename.borrow_mut().invalidate();
        teardown_delete.borrow_mut().invalidate();
    });
    rsx! {
        Flex { direction: "row", sx: header_sx,
            Flex { direction: "row", sx: nav_sx, "aria-label": "Conversation lineage",
                Flex { direction: "row", gap: "xs",
                    Text { color: if online { "success" } else { "error" },
                        Pictogram {
                            icon: pictogram_icons_lucide::circle::outlined,
                            aria_label: if online { "Machine online" } else { "Machine offline" }
                        }
                    }
                    Text { size: "xs", color: "muted", title: "{conversation.worker_label}", "{conversation.worker_label}" }
                    Text { size: "xs", color: "muted", aria_hidden: "true", "›" }
                    Text { size: "xs", color: "muted", title: conversation.cwd.clone(), "{short_path(&conversation.cwd)}" }
                }
                for (index, ancestor) in lineage.into_iter().enumerate() {
                    Text { size: "xs", color: "muted", aria_hidden: "true", "›" }
                    if index + 1 == lineage_len {
                        Text { size: "sm", title: super::display_conversation_title(&display_title.read()),
                            "{super::display_conversation_title(&display_title.read())}"
                        }
                    } else {
                        a {
                            href: crate::routes::agent_href(&ancestor.id),
                            onclick: move |event| { event.prevent_default(); navigate.call(crate::routes::agent_href(&ancestor.id)); },
                            title: super::display_conversation_title(&ancestor.title),
                            "{super::display_conversation_title(&ancestor.title)}"
                        }
                    }
                }
                if !online { Text { size: "xs", color: "error", "Offline" } }
            }
            if let Some(usage) = usage {
                Flex { direction: "row", gap: "xs", "data-testid": "agent-chat-usage",
                    CircularProgress {
                        aria_label: "Context window used",
                        role: "meter",
                        aria_valuetext: usage.title.clone(),
                        title: usage.title.clone(),
                        value: Some(usage.fraction.unwrap_or(0.0) * 100.0),
                        color: if usage.fraction.is_some_and(|fraction| fraction >= 0.92) { "error" }
                            else if usage.fraction.is_some_and(|fraction| fraction >= 0.75) { "warning" }
                            else { "primary" },
                        size: "xs",
                        max: 100.0,
                    }
                    if let Some(fraction) = usage.fraction { Text { size: "xs", color: "muted", "{(fraction * 100.0).round() as i64}%" } }
                }
            }
            Flex { direction: "row", sx: controls_sx,
                ActionIcon {
                    variant: "standard",
                    aria_label: "Rename conversation",
                    title: "Rename conversation",
                    onclick: move |_| {
                        rename_title.set(display_title());
                        rename_error.set(None);
                        rename_open.set(true);
                        let opening_generation = rename_generation.borrow_mut().begin();
                        let mut rename_open = rename_open;
                        let generation = Rc::clone(&rename_generation);
                        let mut display_title = display_title;
                        rename_modal.open_with(opening_generation).onresult(move |result| {
                            if generation.borrow_mut().finish(opening_generation) {
                                rename_open.set(false);
                                rename_pending.set(false);
                                if let Some(title) = result { display_title.set(title); }
                            }
                        });
                    },
                    Pictogram { icon: pictogram_icons_lucide::pencil::outlined }
                }
                ActionIcon {
                    variant: "standard",
                    aria_label: "Browse conversations",
                    title: "Browse conversations",
                    onclick: move |_| {
                        picker_open.set(true);
                        let opening_generation = picker_generation.borrow_mut().begin();
                        let mut picker_open = picker_open;
                        let generation = Rc::clone(&picker_generation);
                        let navigate = navigate;
                        picker_modal.open().onresult(move |result| {
                            if generation.borrow_mut().finish(opening_generation) {
                                picker_open.set(false);
                                if let Some(href) = result { navigate.call(href); }
                            }
                        });
                    },
                    Pictogram { icon: pictogram_icons_lucide::chevron_down::outlined }
                }
                ToolbarMenu {
                    menu_key: "actions",
                    trigger: ToolbarTrigger::Icon { icon: pictogram_icons_lucide::ellipsis::outlined },
                    aria_label: "Conversation actions",
                    items: vec![ToolbarMenuItem {
                        danger: true,
                        ..ToolbarMenuItem::choice("delete", "Delete conversation", false)
                    }],
                    on_choose: move |_| {
                        confirm_delete.set(true);
                        let opening_generation = delete_generation.borrow_mut().begin();
                        let mut confirm_delete = confirm_delete;
                        let generation = Rc::clone(&delete_generation);
                        let pump = pump.clone();
                        let conversation_id = conversation_id.clone();
                        delete_modal.open().onresult(move |result| {
                            if generation.borrow_mut().finish(opening_generation) {
                                confirm_delete.set(false);
                                if result == Some(true) {
                                    delete_conversation(pump.clone(), conversation_id.clone());
                                }
                            }
                        });
                    },
                }
            }
        }
    }
}
#[cfg(target_arch = "wasm32")]
fn rename_conversation_title(
    pump: Pump,
    conversation_id: String,
    title: String,
    opening_generation: u64,
    generation: Rc<RefCell<OverlayGeneration>>,
    mut pending: Signal<bool>,
    mut error_message: Signal<Option<String>>,
    mut display_title: Signal<String>,
    scope: ModalScope<u64, String>,
) {
    spawn(async move {
        let saved_title = title.clone();
        let result = pump
            .rpc()
            .call(&ConfigureAgentChat {
                conversation_id,
                title: Some(title),
                ..Default::default()
            })
            .await
            .map(|_| ());
        if !generation.borrow().is_current(opening_generation) {
            return;
        }
        pending.set(false);
        match result {
            Ok(()) => {
                display_title.set(saved_title.clone());
                scope.resolve(saved_title);
            }
            Err(problem) => error_message.set(Some(problem.to_string())),
        }
    });
}

#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
fn rename_conversation_title(
    _: Pump,
    _: String,
    _: String,
    _: u64,
    _: Rc<RefCell<OverlayGeneration>>,
    mut pending: Signal<bool>,
    mut error_message: Signal<Option<String>>,
    _: Signal<String>,
    _: ModalScope<u64, String>,
) {
    pending.set(false);
    error_message.set(Some(
        "Renaming conversations requires the browser client.".to_owned(),
    ));
}
#[cfg(target_arch = "wasm32")]
fn delete_conversation(pump: Pump, conversation_id: String) {
    crate::components::deck::close_agent_tab::close_agent_tab(pump, conversation_id);
}

#[cfg(not(target_arch = "wasm32"))]
fn delete_conversation(_: Pump, _: String) {}
