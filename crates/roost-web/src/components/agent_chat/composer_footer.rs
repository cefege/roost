//! The footer keeps mode controls and existing agent settings beside the input.
//! It exposes the busy-send choice and foreground interrupt while preserving
//! model selection, voice controls, planning, advisor and goal actions.
//! Commands continue through `composer_commands`.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{ConversationSummary, GoalState, ModelsCatalog};

use super::composer_commands::{advisor_title, report_command, submit_direct_command};
use super::goal_dock::GoalDock;
use super::model_controls::AgentModelControls;
use super::toolbar_menu::{ToolbarMenu, ToolbarMenuItem, ToolbarTrigger};
use crate::components::ui::{
    Button, ButtonSize, ButtonVariant, Chip, Icon, IconButton, IconType, InputPromptFooter,
    InputPromptSubmit, InputPromptTools,
};
use crate::pump::Pump;

#[component]
pub(super) fn ComposerFooter(
    conversation: ConversationSummary,
    catalog: Option<ModelsCatalog>,
    catalog_failed: bool,
    pump: Pump,
    goal: Option<GoalState>,
    goal_view_request: Signal<String>,
    on_goal_open: EventHandler<()>,
    command_error: Signal<Option<String>>,
    commands_open: bool,
    listbox: String,
    on_commands: EventHandler<()>,
    running: bool,
    has_draft: bool,
    send_available: bool,
    voice_controls: Element,
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
    let plan_pump = pump.clone();
    let plan_conversation = conversation.id.clone();
    let advisor_pump = pump.clone();
    let advisor_conversation = conversation.id.clone();
    rsx! {
        InputPromptFooter {
            InputPromptTools { class: "min-w-0 flex-wrap",
                IconButton {
                    icon: IconType::Plus,
                    label: "Show agent commands",
                    title: "Commands",
                    variant: ButtonVariant::Ghost,
                    "data-testid": "agent-chat-commands",
                    expanded: Some(commands_open),
                    controls_id: Some(listbox),
                    onclick: move |_| on_commands.call(()),
                }
                if conversation.mode == "plan" {
                    Chip { label: "Plan".to_string(), icon: Some(IconType::ClipboardList), selected: Some(true),
                        title: Some("Plan mode is on. Select to turn it off.".to_string()),
                        onclick: Some(EventHandler::new(move |_| {
                            submit_direct_command(plan_pump.clone(), plan_conversation.clone(), "/plan".to_string(), report)
                        })),
                    }
                }
                if conversation.advisor || advisor_pending() {
                    Chip { label: "Advisor".to_string(), icon: Some(IconType::Brain), selected: Some(conversation.advisor),
                        title: Some(advisor_title(conversation.advisor, advisor_pending()).to_string()),
                        test_id: Some("agent-advisor-toggle".to_string()),
                        disabled: advisor_pending(),
                        busy: advisor_pending(),
                        onclick: Some(EventHandler::new(move |_| {
                            if advisor_pending() {
                                return;
                            }
                            advisor_pending.set(true);
                            submit_direct_command(advisor_pump.clone(), advisor_conversation.clone(), "/advisor".to_string(), advisor_done);
                        })),
                    }
                }
                GoalDock { goal, view_request: goal_view_request, on_open: on_goal_open }
            }
            InputPromptTools { class: "min-w-0 flex-wrap justify-end",
                AgentModelControls { conversation: conversation.clone(), catalog, catalog_failed, pump }
                {voice_controls}
                {
                    let mode = busy_mode();
                    let label = match mode.as_str() {
                        "steer" => "Steer",
                        "reject" => "Reject if busy",
                        _ => "Follow-up",
                    };
                    let mut items = vec![
                        ToolbarMenuItem::choice("follow_up", "Follow-up", mode == "follow_up"),
                        ToolbarMenuItem::choice("steer", "Steer", mode == "steer"),
                        ToolbarMenuItem::choice("reject", "Reject if busy", mode == "reject"),
                    ];
                    for item in &mut items {
                        item.detail = Some(match item.value.as_str() {
                            "steer" => "Interrupt current turn".to_owned(),
                            "reject" => "Do not queue".to_owned(),
                            _ => "Queue after current turn".to_owned(),
                        });
                    }
                    rsx! {
                        ToolbarMenu {
                            menu_key: "when-busy",
                            trigger: ToolbarTrigger::Picker { label: label.to_owned() },
                            aria_label: "When busy",
                            items,
                            on_choose: move |mode| busy_mode.set(mode),
                            opens_up: true,
                        }
                    }
                }
                if running {
                    Button {
                        icon: IconType::Square,
                        size: ButtonSize::Sm,
                        "aria-label": "Interrupt foreground work",
                        title: "Interrupt foreground work. Background jobs continue.",
                        variant: ButtonVariant::Destructive,
                        "data-testid": "agent-chat-stop",
                        onclick: move |_| on_stop.call(()),
                        "Interrupt"
                    }
                }
                if send_available {
                    InputPromptSubmit {
                        disabled: disabled || !has_draft || submit_pending,
                        aria_label: if running {
                            match busy_mode().as_str() {
                                "follow_up" => "Send follow-up",
                                "reject" => "Reject if busy",
                                _ => "Steer the agent",
                            }
                        } else { "Send message" },
                        title: if running {
                            match busy_mode().as_str() {
                                "follow_up" => "Follow-up",
                                "reject" => "Reject if busy",
                                _ => "Steer",
                            }
                        } else { "Send" },
                        test_id: "agent-chat-send",
                        onclick: move |_| on_send.call(()),
                        Icon { icon: IconType::ArrowUp, class: "size-4" }
                    }
                }
            }
        }
    }
}
