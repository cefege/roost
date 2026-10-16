//! The composer's bottom row, laid out as chat composers lay theirs out: a `+`
//! that opens the command menu and the modes that are on (plan, advisor, a
//! goal) on the left; the model, thinking effort and send or stop on the right.
//! Mounted by `composer`; commands it starts go through `composer_commands`.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{ConversationSummary, GoalState, ModelsCatalog};

use super::composer_commands::{advisor_title, report_command, submit_direct_command};
use super::goal_dock::GoalDock;
use super::model_controls::AgentModelControls;
use crate::components::md::{ButtonVariant, Chip, IconButton, IconButtonSize};
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
    /// Whether the draft holds something to send.
    has_draft: bool,
    disabled: bool,
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
        div { class: "agent-chat__composer-footer",
            div { class: "agent-chat__composer-tools",
                IconButton {
                    icon: "add",
                    label: "Show agent commands",
                    title: "Commands",
                    variant: ButtonVariant::Ghost,
                    size: IconButtonSize::Icon,
                    "data-testid": "agent-chat-commands",
                    expanded: Some(commands_open),
                    controls_id: Some(listbox),
                    onclick: move |_| on_commands.call(()),
                }
                if conversation.mode == "plan" {
                    Chip { label: "Plan".to_string(), icon: Some("assignment".to_string()), selected: Some(true),
                        title: Some("Plan mode is on. Select to turn it off.".to_string()),
                        onclick: Some(EventHandler::new(move |_| {
                            submit_direct_command(plan_pump.clone(), plan_conversation.clone(), "/plan".to_string(), report)
                        })),
                    }
                }
                if conversation.advisor || advisor_pending() {
                    Chip { label: "Advisor".to_string(), icon: Some("psychology".to_string()), selected: Some(conversation.advisor),
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
            div { class: "agent-chat__composer-send",
                AgentModelControls { conversation: conversation.clone(), catalog, catalog_failed, pump }
                if running {
                    IconButton {
                        icon: "stop",
                        label: "Stop the agent",
                        title: "Stop",
                        variant: ButtonVariant::Secondary,
                        size: IconButtonSize::Icon,
                        class: "agent-chat__action",
                        "data-testid": "agent-chat-stop",
                        onclick: move |_| on_stop.call(()),
                    }
                }
                if !running || has_draft {
                    IconButton {
                        icon: "arrow_upward",
                        label: if running { "Steer the agent" } else { "Send message" },
                        title: if running { "Steer" } else { "Send" },
                        variant: ButtonVariant::Default,
                        size: IconButtonSize::Icon,
                        class: "agent-chat__action",
                        "data-testid": "agent-chat-send",
                        "aria-disabled": (disabled || !has_draft).then_some("true"),
                        onclick: move |_| on_send.call(()),
                    }
                }
            }
        }
    }
}
