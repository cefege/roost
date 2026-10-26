//! The composer footer: the controls that act on the next message, kept to one
//! non-wrapping row so it fits a 390px phone. Commands, model, plan, voice, the
//! busy-send choice, interrupt and send live here; advisor, goal and usage sit in
//! the status line beneath the composer. Commands continue through `composer_commands`.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{ConversationSummary, ModelsCatalog};

use super::composer_commands::{report_command, submit_direct_command};
use super::model_controls::AgentModelControls;
use super::toolbar_menu::{ToolbarMenu, ToolbarMenuItem, ToolbarTrigger};
use crate::components::ui::{
    ButtonVariant, Chip, Icon, IconButton, IconType, InputPromptFooter, InputPromptSubmit,
    InputPromptTools,
};
use crate::pump::Pump;

#[component]
pub(super) fn ComposerFooter(
    conversation: ConversationSummary,
    catalog: Option<ModelsCatalog>,
    catalog_failed: bool,
    pump: Pump,
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
    let report = report_command(command_error);
    let plan_on = conversation.mode == "plan";
    let plan_title = if plan_on {
        "Plan mode is on. Select to turn it off."
    } else {
        "Plan mode is off. Select to turn it on."
    };
    let plan_pump = pump.clone();
    let plan_conversation = conversation.id.clone();
    let ready = !(disabled || !has_draft || submit_pending);
    rsx! {
        InputPromptFooter {
            InputPromptTools { class: "min-w-0 flex-1 flex-nowrap overflow-hidden",
                IconButton {
                    class: "shrink-0",
                    icon: IconType::Plus,
                    label: "Show agent commands",
                    title: "Commands",
                    variant: ButtonVariant::Ghost,
                    "data-testid": "agent-chat-commands",
                    expanded: Some(commands_open),
                    controls_id: Some(listbox),
                    onclick: move |_| on_commands.call(()),
                }
                div { class: "min-w-0 flex-1 overflow-hidden",
                    AgentModelControls { conversation: conversation.clone(), catalog, catalog_failed, pump }
                }
                Chip {
                    label: "Plan".to_string(),
                    icon: Some(IconType::ClipboardList),
                    selected: Some(plan_on),
                    class: "shrink-0",
                    title: Some(plan_title.to_string()),
                    onclick: Some(EventHandler::new(move |_| {
                        submit_direct_command(plan_pump.clone(), plan_conversation.clone(), "/plan".to_string(), report)
                    })),
                }
            }
            InputPromptTools { class: "shrink-0 flex-nowrap justify-end",
                {voice_controls}
                if running {
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
                    IconButton {
                        class: "shrink-0",
                        icon: IconType::Square,
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
