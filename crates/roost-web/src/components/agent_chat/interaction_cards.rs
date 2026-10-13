//! Interactive agent transcript cards: the current task list, pending questions
//! and side-question answers. RPC calls stay behind the browser host's pump.
//! The task dock observes committed state; answers use canonical agent RPCs.

use std::collections::BTreeMap;

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{AskQuestion, TodoPhase, TodoStatus};

use crate::components::md::{
    Button, ButtonVariant, Card, CardVariant, Checkbox, Chip, Icon, IconSize, Surface,
    SurfaceElement, SurfaceRadius, TextField,
};
use crate::pump::Pump;

#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::AnswerAgentQuestion;

#[component]
pub fn TodoCard(phases: Vec<TodoPhase>) -> Element {
    let mut expanded = use_signal(|| false);
    let total = phases.iter().map(|phase| phase.tasks.len()).sum::<usize>();
    let closed = phases
        .iter()
        .flat_map(|phase| &phase.tasks)
        .filter(|task| matches!(task.status, TodoStatus::Completed | TodoStatus::Abandoned))
        .count();
    let preview = phases
        .iter()
        .flat_map(|phase| &phase.tasks)
        .find(|task| task.status == TodoStatus::InProgress || task.status == TodoStatus::Blocked)
        .or_else(|| {
            phases
                .iter()
                .flat_map(|phase| &phase.tasks)
                .find(|task| !matches!(task.status, TodoStatus::Completed | TodoStatus::Abandoned))
        });
    rsx! {
        Surface { element: SurfaceElement::Section, class: "agent-chat__todo-hud", test_id: "agent-chat-todo", level: 1, pad: Some(2), border: true, radius: SurfaceRadius::Lg,
            Button {
                variant: ButtonVariant::Link,
                class: "agent-chat__todo-toggle",
                aria_expanded: expanded().to_string(),
                "data-testid": "agent-chat-todo-toggle",
                onclick: move |_| expanded.set(!expanded()),
                Icon { name: "checklist", size: IconSize::Sm }
                strong { "To-do {closed}/{total}" }
                if let Some(task) = preview {
                    span { class: "agent-chat__todo-preview", "{task.content}" }
                    if let Some(blocker) = &task.blocker { span { class: "agent-chat__todo-blocker-preview", "{blocker}" } }
                } else if total == 0 {
                    span { "No tasks" }
                } else {
                    span { "All tasks closed" }
                }
                Icon { name: if expanded() { "expand_less" } else { "expand_more" }, size: IconSize::Sm }
            }
            if expanded() {
                div { class: "agent-chat__todo-list", role: "list", "aria-label": "Phased task checklist",
                    for phase in phases.iter() {
                        details {
                            class: "agent-chat__todo-phase",
                            key: "{phase.name}",
                            open: phase.tasks.iter().any(|task| !matches!(task.status, TodoStatus::Completed | TodoStatus::Abandoned)),
                            summary { class: "agent-chat__todo-phase-title", "{phase.name}" }
                            for (index, task) in phase.tasks.iter().enumerate() {
                                div { class: "agent-chat__todo-task", role: "listitem", key: "{phase.name}-{index}",
                                    Icon { name: task_icon(task.status), size: IconSize::Sm }
                                    span { "{task.content}" }
                                    span { class: "agent-chat__todo-status", "data-status": status_label(task.status).to_ascii_lowercase().replace(' ', "-"), "{status_label(task.status)}" }
                                    if let Some(blocker) = &task.blocker { span { class: "agent-chat__todo-blocker", "{blocker}" } }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn task_icon(status: TodoStatus) -> &'static str {
    match status {
        TodoStatus::Pending => "radio_button_unchecked",
        TodoStatus::InProgress => "pending",
        TodoStatus::Completed => "check_circle",
        TodoStatus::Abandoned => "cancel",
        TodoStatus::Blocked => "block",
    }
}
fn status_label(status: TodoStatus) -> &'static str {
    match status {
        TodoStatus::Pending => "Pending",
        TodoStatus::InProgress => "In progress",
        TodoStatus::Completed => "Completed",
        TodoStatus::Abandoned => "Abandoned",
        TodoStatus::Blocked => "Blocked",
    }
}

#[component]
pub fn AskCard(
    conversation_id: String,
    item_id: String,
    questions: Vec<AskQuestion>,
    answers: Option<BTreeMap<String, Vec<String>>>,
    pump: Pump,
) -> Element {
    let choices = use_signal(BTreeMap::<String, Vec<String>>::new);
    let other = use_signal(BTreeMap::<String, String>::new);
    let error = use_signal(String::new);
    let submitted = answers.is_some();
    let choices_for_send = choices();
    let other_for_send = other();
    let questions_for_send = questions.clone();
    let send_pump = pump.clone();
    let send_conversation = conversation_id.clone();
    let send_item = item_id.clone();
    rsx! {
        Card { title: Some("A question for you".to_string()), variant: CardVariant::Outlined, class: Some("agent-chat__interaction-card".to_string()),
            for question in &questions {
                AskQuestionOptions {
                    key: "{question.id}",
                    question: question.clone(),
                    answers: answers.as_ref().and_then(|values| values.get(&question.id)).cloned(),
                    submitted,
                    choices,
                    other,
                }
            }
            if submitted {
                p { class: "md-body-s", "Your answer has been submitted." }
            } else {
                Button {
                    variant: ButtonVariant::Default,
                    onclick: move |_| submit_answers(send_pump.clone(), send_conversation.clone(), send_item.clone(), questions_for_send.clone(), choices_for_send.clone(), other_for_send.clone(), error),
                    "Submit answers"
                }
            }
            if !error().is_empty() { p { role: "alert", class: "md-body-s", "{error()}" } }
        }
    }
}

#[component]
fn AskQuestionOptions(
    question: AskQuestion,
    answers: Option<Vec<String>>,
    submitted: bool,
    choices: Signal<BTreeMap<String, Vec<String>>>,
    other: Signal<BTreeMap<String, String>>,
) -> Element {
    let question_id = question.id.clone();
    let saved_answers = answers.unwrap_or_default();
    let other_answer = saved_answers
        .iter()
        .find(|value| {
            !question
                .options
                .iter()
                .any(|option| &option.label == *value)
        })
        .cloned();
    let other_value = other_answer
        .or_else(|| other().get(&question_id).cloned())
        .unwrap_or_default();
    rsx! {
        div { class: "agent-chat__ask-question",
            h3 { class: "md-title-s", "{question.question}" }
            for (index, option) in question.options.iter().enumerate() {
                {
                    let selected_label = option.label.clone();
                    let selected_question = question_id.clone();
                    let is_selected = saved_answers.contains(&option.label)
                        || choices().get(&question_id).is_some_and(|values| values.contains(&option.label));
                    let recommended = question.recommended == Some(index);
                    rsx! {
                        div { class: "agent-chat__ask-option", key: "{index}",
                            if question.multi {
                                Checkbox {
                                    checked: is_selected,
                                    disabled: submitted,
                                    label: option.label.clone(),
                                    on_change: move |checked| update_choice(choices, selected_question.clone(), selected_label.clone(), checked, true),
                                }
                            } else {
                                Button {
                                    variant: if is_selected { ButtonVariant::Secondary } else { ButtonVariant::Outline },
                                    disabled: submitted,
                                    onclick: move |_| update_choice(choices, selected_question.clone(), selected_label.clone(), !is_selected, false),
                                    "{option.label}"
                                }
                            }
                            if recommended { Chip { label: "Recommended".to_string(), icon: Some("star".to_string()), selected: None, small: true } }
                            if let Some(description) = &option.description { p { class: "md-body-s", "{description}" } }
                        }
                    }
                }
            }
            TextField {
                label: Some("Other (type your own)".to_string()),
                value: other_value,
                disabled: submitted,
                on_input: move |value| {
                    let mut values = other();
                    values.insert(question_id.clone(), value);
                    other.set(values);
                },
            }
        }
    }
}

fn update_choice(
    mut choices: Signal<BTreeMap<String, Vec<String>>>,
    question: String,
    value: String,
    selected: bool,
    multi: bool,
) {
    let mut values = choices();
    let selected_values = values.entry(question).or_default();
    if !multi {
        selected_values.clear();
    }
    if selected && !selected_values.contains(&value) {
        selected_values.push(value);
    } else if !selected {
        selected_values.retain(|entry| entry != &value);
    }
    choices.set(values);
}

#[allow(unused_mut)]
fn submit_answers(
    pump: Pump,
    conversation_id: String,
    item_id: String,
    questions: Vec<AskQuestion>,
    mut choices: BTreeMap<String, Vec<String>>,
    other: BTreeMap<String, String>,
    mut error: Signal<String>,
) {
    for question in questions {
        if let Some(value) = other
            .get(&question.id)
            .filter(|value| !value.trim().is_empty())
        {
            let selected_values = choices.entry(question.id).or_default();
            if !question.multi {
                selected_values.clear();
            }
            selected_values.push(value.trim().to_string());
        }
    }
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let answers_json = match serde_json::to_string(&choices) {
            Ok(json) => json,
            Err(failure) => {
                error.set(format!("Could not encode answers: {failure}"));
                return;
            }
        };
        match pump
            .rpc()
            .call(&AnswerAgentQuestion {
                conversation_id,
                item_id,
                answers_json,
            })
            .await
        {
            Ok(()) => error.set(String::new()),
            Err(failure) => error.set(format!("Could not submit answers: {failure}")),
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, conversation_id, item_id, choices, error);
}

#[component]
pub fn BtwCard(question: String, answer: Option<String>, error: Option<String>) -> Element {
    rsx! {
        Card { title: Some("Side question".to_string()), variant: CardVariant::Outlined, class: Some("agent-chat__interaction-card".to_string()),
            p { class: "md-body-m", "{question}" }
            if let Some(answer) = answer {
                div { class: "agent-chat__markdown", dangerous_inner_html: super::markdown::markdown_to_safe_html(&answer) }
            } else if let Some(error) = error {
                p { class: "md-body-s", role: "alert", "{error}" }
            } else {
                div { class: "agent-chat__working", role: "status", span { class: "agent-chat__working-mark" }, span { class: "agent-chat__shimmer", "Finding an answer…" } }
            }
        }
    }
}
