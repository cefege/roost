//! The footer keeps mode controls and existing agent settings beside the input.
//! It exposes the busy-send choice and foreground interrupt while preserving
//! model selection, voice controls, planning, advisor and goal actions.
//! Commands continue through `composer_commands`.

use dioxus::prelude::*;
use libero::components::{Button, Chip, Pictogram, Select};
use libero::sx::sx;
use roost_protocol::wire::agent_chat::{ConversationSummary, GoalState, ModelsCatalog};

use super::composer_commands::{advisor_title, report_command, submit_direct_command};
use super::goal_dock::GoalDock;
use super::model_controls::AgentModelControls;
use crate::components::ui::{InputPromptFooter, InputPromptSubmit, InputPromptTools};
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
    /// Whether the command menu is showing options, for the `+` button's state.
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
    let footer_tools_sx = use_hook(|| sx().min_width("0").display("flex").flex_wrap("wrap"));
    let footer_actions_sx = use_hook(|| {
        sx().min_width("0")
            .display("flex")
            .flex_wrap("wrap")
            .justify_content("end")
    });
    rsx! {
        InputPromptFooter {
            InputPromptTools { sx: footer_tools_sx,
                Button {
                    variant: "standard",
                    "aria-label": "Show agent commands",
                    title: "Commands",
                    "aria-expanded": commands_open,
                    "aria-controls": "{listbox}",
                    "data-testid": "agent-chat-commands",
                    onclick: move |_| on_commands.call(()),
                    Pictogram { icon: pictogram_icons_lucide::plus::outlined }
                }
                if conversation.mode == "plan" {
                    Chip {
                        checked: Some(true),
                        icon: Some(rsx! { Pictogram { icon: pictogram_icons_lucide::clipboard_list::outlined } }),
                        onclick: Some(EventHandler::new(move |_| {
                            submit_direct_command(plan_pump.clone(), plan_conversation.clone(), "/plan".to_string(), report)
                        })),
                        "Plan"
                    }
                }
                if conversation.advisor || advisor_pending() {
                    Chip {
                        checked: Some(conversation.advisor),
                        disabled: Some(advisor_pending()),
                        title: Some(advisor_title(conversation.advisor, advisor_pending()).to_string()),
                        icon: Some(rsx! { Pictogram { icon: pictogram_icons_lucide::brain::outlined } }),
                        onclick: Some(EventHandler::new(move |_| {
                            if advisor_pending() {
                                return;
                            }
                            advisor_pending.set(true);
                            submit_direct_command(advisor_pump.clone(), advisor_conversation.clone(), "/advisor".to_string(), advisor_done);
                        })),
                        "Advisor"
                    }
                }
                GoalDock { goal, view_request: goal_view_request, on_open: on_goal_open }
            }
            InputPromptTools { sx: footer_actions_sx,
                AgentModelControls { conversation: conversation.clone(), catalog, catalog_failed, pump }
                {voice_controls}
                Select {
                    value: Some(busy_mode()),
                    onchange: move |mode: Option<String>| {
                        if let Some(mode) = mode {
                            busy_mode.set(mode);
                        }
                    },
                    options: vec!["follow_up".to_string(), "steer".to_string(), "reject".to_string()],
                    label: "When busy",
                }
                if running {
                    Button {
                        variant: "filled",
                        color: "error",
                        icon: Some(rsx! { Pictogram { icon: pictogram_icons_lucide::square::outlined } }),
                        "aria-label": "Interrupt foreground work",
                        title: "Interrupt foreground work. Background jobs continue.",
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
                        Pictogram { icon: pictogram_icons_lucide::arrow_up::outlined, width: "1rem", height: "1rem" }
                    }
                }
            }
        }
    }
}
