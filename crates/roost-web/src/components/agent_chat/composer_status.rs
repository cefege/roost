//! The pinned composer warning for request failures that remain durable in history.
//! Dismissing this surface never removes transcript items or failure records.
//! The active error clears only when the next transcript attempt clears it.

use dioxus::prelude::*;

use super::agent_alert::AgentAlert;
use libero::components::{Button, Collapse, Flex, Text};
use libero::sx::sx;

#[component]
pub(super) fn ComposerErrorBanner(
    error: Option<String>,
    running: bool,
    on_stop: EventHandler<()>,
    on_switch_model: EventHandler<()>,
) -> Element {
    let mut dismissed = use_signal(|| false);
    let mut details_open = use_signal(|| false);
    let mut previous_error = use_signal(|| error.clone());
    use_effect(use_reactive((&error,), move |(current_error,)| {
        if current_error != previous_error.peek().clone() {
            previous_error.set(current_error.clone());
            dismissed.set(false);
            details_open.set(false);
        }
        if current_error.is_none() {
            dismissed.set(false);
        }
    }));
    let banner_sx = use_hook(|| sx().display("flex").flex_direction("column").gap("sm"));
    let heading_row_sx = use_hook(|| {
        sx().display("flex")
            .flex_wrap("wrap")
            .align_items("center")
            .gap("sm")
    });
    let heading_sx = use_hook(|| {
        sx().min_width("0")
            .flex("1")
            .overflow("hidden")
            .with("overscroll-behavior-y", "auto")
            .with("overflow-wrap", "anywhere")
    });
    let details_sx = use_hook(|| {
        sx().margin("0")
            .max_height("12rem")
            .overflow("auto")
            .white_space("pre-wrap")
            .with("overflow-wrap", "anywhere")
            .padding("sm")
    });
    let action_row_sx = use_hook(|| {
        sx().display("flex")
            .flex_wrap("wrap")
            .align_items("center")
            .gap("sm")
    });
    let Some(error_text) = error else {
        return rsx! {};
    };
    if dismissed() {
        return rsx! {};
    }
    let headline = error_headline(&error_text);
    let has_details = error_text.len() > 80 || error_text.lines().count() > 1;
    rsx! {
        AgentAlert { test_id: "agent-chat-pinned-error",
            Flex { sx: banner_sx, direction: "column", gap: "sm",
                Flex { sx: heading_row_sx, direction: "row", align: "center", gap: "sm",
                    Text { sx: heading_sx, size: "sm", "{headline}" }
                    if has_details {
                        Button {
                            variant: "text",
                            "data-testid": "agent-chat-error-details",
                            "aria-expanded": details_open(),
                            "aria-controls": "agent-chat-pinned-error-details",
                            onclick: move |_| details_open.toggle(),
                            if details_open() { "Less" } else { "Details" }
                        }
                    }
                    Button {
                        variant: "text",
                        "data-testid": "agent-chat-error-dismiss",
                        onclick: move |_| dismissed.set(true),
                        "Dismiss"
                    }
                }
                Collapse {
                    id: "agent-chat-pinned-error-details",
                    open: details_open() && has_details,
                    Text { component: "pre", sx: details_sx, size: "xs", color: "ink", "{error_text}" }
                }
            }
            Flex { sx: action_row_sx, direction: "row", align: "center", gap: "sm",
                if running {
                    Button {
                        variant: "filled",
                        color: "error",
                        "data-testid": "agent-chat-error-stop",
                        title: "Interrupt foreground work. Background jobs continue.",
                        onclick: move |_| on_stop.call(()),
                        "Interrupt"
                    }
                }
                Button {
                    variant: "outlined",
                    "data-testid": "agent-chat-error-switch-model",
                    onclick: move |_| on_switch_model.call(()),
                    "Switch model"
                }
            }
        }
    }
}

fn error_headline(error: &str) -> String {
    error
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("The request failed.")
        .to_owned()
}
