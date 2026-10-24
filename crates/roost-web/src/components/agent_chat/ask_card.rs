//! Pending and answered agent questions with their canonical answer submission.
//! Transcript projection supplies the questions and committed answer state;
//! the browser pump owns the RPC while this card owns local selections.

use std::collections::BTreeMap;

use crate::pump::Pump;
use dioxus::prelude::*;
use libero::components::{Box, Button, Checkbox, Chip, Flex, Paper, Pictogram, Text, TextField};
use roost_protocol::wire::agent_chat::AskQuestion;

#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::AnswerAgentQuestion;

use libero::sx::sx;

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
    let card_sx = use_hook(|| sx().display("flex").flex_direction("column").gap("md"));
    let questions_sx = use_hook(|| sx().display("flex").flex_direction("column").gap("md"));
    let question_title_sx = use_hook(|| sx().font_weight("600"));
    let status_sx = use_hook(|| sx().font_size("sm").color("muted"));
    let error_sx = use_hook(|| sx().font_size("sm").color("error"));
    rsx! {
        Paper { sx: card_sx,
            Text { size: "lg", sx: question_title_sx, "A question for you" }
            Flex { direction: "column", sx: questions_sx,
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
                    Text { component: "p", sx: status_sx, "Your answer has been submitted." }
                } else {
                    Button {
                        variant: "filled",
                        onclick: move |_| submit_answers(send_pump.clone(), send_conversation.clone(), send_item.clone(), questions_for_send.clone(), choices_for_send.clone(), other_for_send.clone(), error),
                        "Submit answers"
                    }
                }
                if !error().is_empty() { Text { sx: error_sx, "{error()}" } }
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
    let option_group_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .gap("sm")
            .min_width("0")
    });
    let question_sx = use_hook(|| sx().font_size("sm").font_weight("500"));
    let option_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .align_items("flex-start")
            .gap("xs")
    });
    let checkbox_row_sx = use_hook(|| {
        sx().display("flex")
            .align_items("center")
            .gap("sm")
            .font_size("sm")
            .font_weight("normal")
            .with("line-height", "normal")
    });
    let description_sx = use_hook(|| sx().font_size("sm").color("muted"));
    rsx! {
        Flex { direction: "column", sx: option_group_sx,
            Text { component: "h3", sx: question_sx, "{question.question}" }
            for (index, option) in question.options.iter().enumerate() {
                {
                    let selected_label = option.label.clone();
                    let selected_question = question_id.clone();
                    let is_selected = saved_answers.contains(&option.label)
                        || choices().get(&question_id).is_some_and(|values| values.contains(&option.label));
                    let recommended = question.recommended == Some(index);
                    rsx! {
                        Flex { key: "{index}", direction: "column", sx: option_sx.clone(),
                            if question.multi {
                                Box { component: "label", sx: checkbox_row_sx.clone(),
                                    Checkbox {
                                        checked: Some(is_selected),
                                        disabled: submitted,
                                        aria_label: option.label.clone(),
                                        onchange: move |checked| update_choice(choices, selected_question.clone(), selected_label.clone(), checked, true),
                                    }
                                    Text { component: "span", "{option.label}" }
                                }
                            } else {
                                Button {
                                    variant: if is_selected { "tonal" } else { "outlined" },
                                    disabled: submitted,
                                    onclick: move |_| update_choice(choices, selected_question.clone(), selected_label.clone(), !is_selected, false),
                                    "{option.label}"
                                }
                            }
                            if recommended {
                                Chip { icon: Some(rsx! { Pictogram { icon: pictogram_icons_lucide::star::outlined } }), "Recommended" }
                            }
                            if let Some(description) = &option.description {
                                Text { component: "p", sx: description_sx.clone(), "{description}" }
                            }
                        }
                    }
                }
            }
            TextField {
                label: "Other (type your own)",
                value: Some(other_value),
                disabled: submitted,
                oninput: move |value| {
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
