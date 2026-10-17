//! Interactive agent transcript cards: the current task list, pending questions
//! and side-question answers. RPC calls stay behind the browser host's pump.
//! The task dock observes committed state; answers use canonical agent RPCs.

use std::collections::BTreeMap;

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{AskQuestion, TodoPhase, TodoStatus};

use super::agent_alert::ERROR_TEXT_CLASS;
use crate::components::ui::{
    Button, ButtonVariant, Card, CardContent, CardHeader, CardTitle, CardVariant, Checkbox, Chip,
    Icon, IconType, ListRow, SectionTitle, Shimmer, StatusDot, Surface, SurfaceElement, TextField,
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
        Surface { element: SurfaceElement::Section, class: Some("flex flex-col gap-2 min-w-0".to_owned()), test_id: Some("agent-chat-todo".to_owned()), level: 1, pad: Some(2), border: true,
            Button {
                variant: ButtonVariant::Link,
                class: Some("h-auto min-w-0 justify-start gap-2 whitespace-normal text-left".to_owned()),
                "aria-expanded": expanded().to_string(),
                "data-testid": "agent-chat-todo-toggle",
                onclick: move |_| expanded.set(!expanded()),
                Icon { icon: IconType::ListChecks, class: "size-4" }
                strong { class: "font-medium", "To-do {closed}/{total}" }
                if let Some(task) = preview {
                    span { class: "min-w-0 truncate text-muted-foreground", "{task.content}" }
                    if let Some(blocker) = &task.blocker { span { class: "text-xs text-destructive", "{blocker}" } }
                } else if total == 0 {
                    span { class: "text-muted-foreground", "No tasks" }
                } else {
                    span { class: "text-muted-foreground", "All tasks closed" }
                }
                Icon { icon: if expanded() { IconType::ChevronUp } else { IconType::ChevronDown }, class: "size-4" }
            }
            if expanded() {
                div { class: "flex flex-col gap-2", role: "list", "aria-label": "Phased task checklist",
                    for phase in phases.iter() {
                        details {
                            class: "flex flex-col gap-1",
                            key: "{phase.name}",
                            open: phase.tasks.iter().any(|task| !matches!(task.status, TodoStatus::Completed | TodoStatus::Abandoned)),
                            summary { class: "cursor-pointer list-none", SectionTitle { "{phase.name}" } }
                            for (index, task) in phase.tasks.iter().enumerate() {
                                ListRow {
                                    key: "{phase.name}-{index}",
                                    dense: true,
                                    role: "listitem",
                                    class: Some("min-w-0".to_owned()),
                                    leading_icon: task_icon(task.status),
                                    headline: rsx! { "{task.content}" },
                                    support: task.blocker.as_ref().map(|blocker| rsx! { span { class: "text-xs text-destructive", "{blocker}" } }),
                                    trailing: rsx! { span { class: "text-xs text-muted-foreground", "data-status": status_label(task.status).to_ascii_lowercase().replace(' ', "-"), "{status_label(task.status)}" } },
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn task_icon(status: TodoStatus) -> IconType {
    match status {
        TodoStatus::Pending => IconType::Circle,
        TodoStatus::InProgress => IconType::Loader,
        TodoStatus::Completed => IconType::CircleCheck,
        TodoStatus::Abandoned => IconType::CircleX,
        TodoStatus::Blocked => IconType::Ban,
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
        Card { variant: CardVariant::Outlined,
            CardHeader { CardTitle { "A question for you" } }
            CardContent { class: "flex flex-col gap-3",
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
                    p { class: "text-sm text-muted-foreground", "Your answer has been submitted." }
                } else {
                    Button {
                        variant: ButtonVariant::Default,
                        onclick: move |_| submit_answers(send_pump.clone(), send_conversation.clone(), send_item.clone(), questions_for_send.clone(), choices_for_send.clone(), other_for_send.clone(), error),
                        "Submit answers"
                    }
                }
                if !error().is_empty() { p { role: "alert", class: ERROR_TEXT_CLASS, "{error()}" } }
            }
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
        div { class: "flex flex-col gap-2 min-w-0",
            h3 { class: "text-sm font-medium", "{question.question}" }
            for (index, option) in question.options.iter().enumerate() {
                {
                    let selected_label = option.label.clone();
                    let selected_question = question_id.clone();
                    let is_selected = saved_answers.contains(&option.label)
                        || choices().get(&question_id).is_some_and(|values| values.contains(&option.label));
                    let recommended = question.recommended == Some(index);
                    rsx! {
                        div { class: "flex flex-col items-start gap-1", key: "{index}",
                            if question.multi {
                                label { class: "flex items-center gap-2 text-sm",
                                    Checkbox {
                                        checked: is_selected,
                                        disabled: submitted,
                                        aria_label: option.label.clone(),
                                        on_checked_change: move |checked| update_choice(choices, selected_question.clone(), selected_label.clone(), checked, true),
                                    }
                                    span { "{option.label}" }
                                }
                            } else {
                                Button {
                                    variant: if is_selected { ButtonVariant::Secondary } else { ButtonVariant::Outline },
                                    disabled: submitted,
                                    onclick: move |_| update_choice(choices, selected_question.clone(), selected_label.clone(), !is_selected, false),
                                    "{option.label}"
                                }
                            }
                            if recommended { Chip { label: "Recommended".to_string(), icon: Some(IconType::Star), selected: None, small: true } }
                            if let Some(description) = &option.description { p { class: "text-sm text-muted-foreground", "{description}" } }
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
                request_id: super::request_ids::new_request_id(),
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
    let loading = use_signal(|| true);
    rsx! {
        Card { variant: CardVariant::Outlined,
            CardHeader { CardTitle { "Side question" } }
            CardContent { class: "flex flex-col gap-3",
                p { class: "text-sm text-foreground", "{question}" }
                if let Some(answer) = answer {
                    div { class: "text-sm text-foreground break-words", dangerous_inner_html: super::markdown::markdown_to_safe_html(&answer) }
                } else if let Some(error) = error {
                    p { class: ERROR_TEXT_CLASS, role: "alert", "{error}" }
                } else {
                    div { class: "flex items-center gap-2 text-sm text-muted-foreground", role: "status",
                        StatusDot { status: "running".to_string() }
                        Shimmer { loading, span { "Finding an answer…" } }
                    }
                }
            }
        }
    }
}
