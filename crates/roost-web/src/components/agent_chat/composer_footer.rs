//! The Rust/UI prompt footer keeps commands and attachments on the left, model
//! and send controls on the right. Narrow panes wrap without clipping controls;
//! planning, advisor, goal and busy-send policy share one options menu.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{ConversationSummary, GoalState, ModelsCatalog};

use super::composer_attachments::AgentAttachmentControls;
use super::composer_commands::{report_command, submit_direct_command};
use super::model_controls::AgentModelControls;
use super::toolbar_menu::{ToolbarMenu, ToolbarMenuItem, ToolbarTrigger};
use crate::components::terminal_chrome::attachment_picker::ChosenFile;
use crate::components::ui::input_group::{InputGroupAddon, InputGroupAddonAlign};
use crate::components::ui::{
    ButtonSize, ButtonVariant, Icon, IconButton, IconType, InputPromptSubmit, InputPromptTools,
};
use crate::pump::Pump;

#[component]
pub(super) fn ComposerFooter(
    conversation: ConversationSummary,
    catalog: Option<ModelsCatalog>,
    catalog_failed: bool,
    pump: Pump,
    goal: Option<GoalState>,
    mut goal_view_request: Signal<String>,
    on_goal_open: EventHandler<()>,
    command_error: Signal<Option<String>>,
    commands_open: bool,
    listbox: String,
    on_commands: EventHandler<()>,
    running: bool,
    has_draft: bool,
    send_available: bool,
    dictation_finishing: bool,
    voice_controls: Element,
    staged_files: Signal<Vec<ChosenFile>>,
    on_chosen: EventHandler<Vec<ChosenFile>>,
    uploads_in_flight: bool,
    attachments_block_send: bool,
    refining_plan: bool,
    refinement_has_attachments: bool,
    disabled: bool,
    busy_mode: Signal<String>,
    submit_pending: bool,
    on_send: EventHandler<()>,
    on_stop: EventHandler<()>,
) -> Element {
    let mut advisor_pending = use_signal(|| false);
    let mut command_error = command_error;
    let advisor_done = EventHandler::new(move |result: Result<(), String>| {
        advisor_pending.set(false);
        command_error.set(result.err());
    });
    let report = report_command(command_error);
    let options_pump = pump.clone();
    let options_conversation = conversation.id.clone();
    let mode = busy_mode();
    let mut options = vec![
        ToolbarMenuItem::choice("plan", "Plan mode", conversation.mode == "plan"),
        ToolbarMenuItem::choice("advisor", "Advisor", conversation.advisor),
    ];
    if let Some(goal) = goal.filter(|goal| !matches!(goal.status.as_str(), "complete" | "dropped"))
    {
        options.push(ToolbarMenuItem {
            detail: Some(goal.status.as_str().to_owned()),
            ..ToolbarMenuItem::choice("goal", "Manage goal", false)
        });
    }
    for (value, label, detail) in [
        ("follow_up", "Follow-up", "Queue when busy"),
        ("steer", "Steer", "Interrupt when busy"),
        ("reject", "Reject if busy", "Do not queue"),
    ] {
        options.push(ToolbarMenuItem {
            detail: Some(detail.to_owned()),
            ..ToolbarMenuItem::choice(value, label, mode == value)
        });
    }
    let ready = !(disabled
        || submit_pending
        || attachments_block_send
        || refinement_has_attachments
        || (!has_draft && !dictation_finishing));
    rsx! {
        InputGroupAddon {
            align: InputGroupAddonAlign::BlockEnd,
            class: "flex-wrap gap-y-2 border-t-0 px-2 pb-2 pt-0",
            InputPromptTools { class: "min-w-0 max-w-full flex-wrap",
                IconButton {
                    class: "shrink-0",
                    icon: IconType::Plus,
                    label: "Show agent commands",
                    title: "Commands",
                    variant: ButtonVariant::Ghost,
                    size: ButtonSize::IconSm,
                    "data-testid": "agent-chat-commands",
                    expanded: Some(commands_open),
                    controls_id: Some(listbox),
                    onclick: move |_| on_commands.call(()),
                }
                AgentAttachmentControls {
                    pump: pump.clone(),
                    conversation: conversation.clone(),
                    staged_files,
                    on_chosen,
                    disabled,
                }
            }
            InputPromptTools { class: "order-first ml-auto min-w-0 w-full flex-nowrap justify-end sm:order-none sm:w-auto",
                ToolbarMenu {
                    menu_key: "options",
                    trigger: ToolbarTrigger::Icon { icon: IconType::Ellipsis },
                    aria_label: "Chat options",
                    items: options,
                    opens_up: true,
                    on_choose: move |choice: String| match choice.as_str() {
                        "plan" => submit_direct_command(options_pump.clone(), options_conversation.clone(), "/plan".to_owned(), report),
                        "advisor" => {
                            if !advisor_pending() {
                                advisor_pending.set(true);
                                submit_direct_command(options_pump.clone(), options_conversation.clone(), "/advisor".to_owned(), advisor_done);
                            }
                        }
                        "goal" => {
                            goal_view_request.set(String::new());
                            on_goal_open.call(());
                        }
                        _ => busy_mode.set(choice),
                    },
                }
                AgentModelControls { conversation: conversation.clone(), catalog, catalog_failed, pump }
            }
            InputPromptTools { class: "ml-auto min-w-0 flex-nowrap justify-end sm:ml-0",
                {voice_controls}
                if running {
                    IconButton {
                        icon: IconType::Square,
                        size: ButtonSize::IconSm,
                        label: "Interrupt foreground work",
                        title: "Interrupt foreground work. Background jobs continue.",
                        variant: ButtonVariant::Destructive,
                        "data-testid": "agent-chat-stop",
                        onclick: move |_| on_stop.call(()),
                    }
                }
                if send_available {
                    InputPromptSubmit {
                        disabled: !ready,
                        variant: if ready { ButtonVariant::Default } else { ButtonVariant::Ghost },
                        aria_label: if refining_plan {
                            "Send plan feedback"
                        } else if running {
                            match busy_mode().as_str() {
                                "follow_up" => "Send follow-up",
                                "reject" => "Reject if busy",
                                _ => "Steer the agent",
                            }
                        } else {
                            "Send message"
                        },
                        title: if uploads_in_flight {
                            "Uploading…"
                        } else if refinement_has_attachments {
                            "Remove attachments to send feedback"
                        } else if refining_plan {
                            "Send feedback"
                        } else if running {
                            match busy_mode().as_str() {
                                "follow_up" => "Follow-up",
                                "reject" => "Reject if busy",
                                _ => "Steer",
                            }
                        } else {
                            "Send"
                        },
                        test_id: "agent-chat-send",
                        onclick: move |_| on_send.call(()),
                        Icon { icon: IconType::ArrowUp, class: "size-4" }
                    }
                }
            }
        }
    }
}
