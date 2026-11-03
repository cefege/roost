//! One ACP form field, with OMP's labels, descriptions and typed choices.
//! The interaction card owns answers; this component keeps unfinished numeric input.
//! Uses stock form controls and the original schema without native-agent assumptions.

use crate::components::ui::{
    Button, ButtonSize, ButtonVariant, Checkbox, Field, FieldDescription, FieldLabel, Input,
    InputType, Textarea,
    field::{FieldContent, FieldTitle, FieldVariant},
};
use dioxus::prelude::*;
use serde_json::{Map, Value, json};

struct FormChoice {
    value: Value,
    label: String,
    description: Option<String>,
}

#[component]
pub(super) fn OmpFormField(
    name: String,
    schema: Value,
    required: bool,
    content: Signal<Map<String, Value>>,
    disabled: bool,
) -> Element {
    let mut content = content;
    let control_id = use_hook(|| crate::components::ui::field::scoped_element_id("omp-answer"));
    let description_id = crate::components::ui::field::description_id(&control_id);
    let title = schema
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            if name == "value" {
                "Your answer"
            } else {
                name.as_str()
            }
        });
    let description = schema
        .get("description")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let kind = schema
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let current = content.read().get(&name).cloned().unwrap_or(Value::Null);
    let numeric = matches!(kind, "number" | "integer");
    let mut numeric_draft = use_signal(|| {
        if numeric {
            input_text(&current)
        } else {
            String::new()
        }
    });
    // OMP's __other siblings are alternatives; blank editors need not crowd out the questions.
    let custom_answer = kind == "string" && !required && name.ends_with("__other");
    let mut custom_answer_open =
        use_signal(|| current.as_str().is_some_and(|value| !value.is_empty()));
    let choices = form_choices(&schema);
    let input_kind = kind.to_owned();
    let description_ref = description.as_ref().map(|_| description_id.clone());
    rsx! {
        if custom_answer {
            {
                let custom_name = name.clone();
                rsx! {
                    Button {
                        variant: ButtonVariant::Ghost,
                        size: ButtonSize::Sm,
                        class: "self-start",
                        disabled,
                        "aria-expanded": custom_answer_open().to_string(),
                        "aria-controls": control_id.clone(),
                        onclick: move |_| {
                            let closing = custom_answer_open();
                            if closing { content.write().remove(&custom_name); }
                            custom_answer_open.set(!closing);
                        },
                        "{title}"
                    }
                }
            }
        }
        if !custom_answer || custom_answer_open() {
        Field { class: "min-w-0", disabled,
            if choices.is_empty() && !custom_answer {
                FieldLabel { html_for: control_id.clone(), "{title}" if required { " · required" } }
            } else if !choices.is_empty() {
                FieldTitle { "{title}" if required { " · required" } }
            }
            if let Some(description) = description.as_ref() {
                FieldDescription { id: description_id, "{description}" }
            }
            if !choices.is_empty() {
                div { class: "flex min-w-0 flex-col gap-2", role: "group", "aria-label": title,
                    for (index, choice) in choices.into_iter().enumerate() {
                        {
                            let option_id = format!("{control_id}-{index}");
                            let option_description = crate::components::ui::field::description_id(&option_id);
                            let described_by = choice.description.as_ref().map(|_| option_description.clone());
                            let field = name.clone();
                            let value = choice.value;
                            rsx! {
                                div { key: "{index}", class: "flex min-w-0 flex-col gap-1",
                                    if kind == "array" {
                                        Field { variant: FieldVariant::Horizontal, disabled,
                                            Checkbox {
                                                id: option_id.clone(),
                                                checked: current.as_array().is_some_and(|values| values.contains(&value)),
                                                aria_label: choice.label.clone(),
                                                aria_described_by: described_by,
                                                disabled,
                                                on_checked_change: move |_| update_answer(content, &field, |current| toggle_array(current, &value)),
                                            }
                                            FieldContent {
                                                FieldLabel { html_for: option_id, "{choice.label}" }
                                                if let Some(description) = choice.description {
                                                    FieldDescription { id: option_description, "{description}" }
                                                }
                                            }
                                        }
                                    } else {
                                        Button {
                                            class: "h-auto max-w-full justify-start whitespace-normal text-left",
                                            size: ButtonSize::Sm,
                                            variant: if current == value { ButtonVariant::Default } else { ButtonVariant::Outline },
                                            "aria-pressed": (current == value).to_string(),
                                            "aria-describedby": described_by,
                                            disabled,
                                            onclick: move |_| update_answer(content, &field, |_| value.clone()),
                                            "{choice.label}"
                                        }
                                        if let Some(description) = choice.description {
                                            FieldDescription { id: option_description, "{description}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            } else if kind == "boolean" {
                Checkbox {
                    id: control_id,
                    checked: current.as_bool().unwrap_or(false),
                    aria_label: title.to_owned(),
                    aria_described_by: description_ref,
                    disabled,
                    on_checked_change: move |checked| update_answer(content, &name, |_| Value::Bool(checked)),
                }
            } else if kind == "string" {
                // ACP gives input() and multiline editor() the same string schema.
                Textarea {
                    id: control_id,
                    aria_label: title.to_owned(),
                    aria_describedby: description_ref,
                    value: input_text(&current),
                    placeholder: description.clone(),
                    rows: 1,
                    required,
                    disabled,
                    onmounted: move |event: MountedEvent| {
                        if custom_answer {
                            spawn(async move { let _ = event.data().set_focus(true).await; });
                        }
                    },
                    oninput: move |event: FormEvent| update_answer(content, &name, |_| Value::String(event.value())),
                }
            } else {
                Input {
                    id: control_id,
                    aria_label: title.to_owned(),
                    aria_describedby: description_ref,
                    r#type: InputType::Text,
                    inputmode: numeric.then(|| if input_kind == "integer" { "numeric" } else { "decimal" }.to_owned()),
                    value: if numeric { numeric_draft() } else { input_text(&current) },
                    placeholder: description.clone(),
                    required,
                    disabled,
                    oninput: move |event: FormEvent| {
                        let text = event.value();
                        let value = match input_kind.as_str() {
                            "integer" => text.parse::<serde_json::Number>().ok().filter(|number| number.is_i64() || number.is_u64()).map(Value::Number).unwrap_or(Value::Null),
                            "number" => text.parse::<f64>().ok().and_then(serde_json::Number::from_f64).map(Value::Number).unwrap_or(Value::Null),
                            _ => Value::String(text.clone()),
                        };
                        let absent = numeric && text.is_empty();
                        if numeric { numeric_draft.set(text); }
                        if absent {
                            content.write().remove(&name);
                        } else {
                            update_answer(content, &name, |_| value);
                        }
                    },
                }
            }
        }
        }
    }
}

fn input_text(value: &Value) -> String {
    value.as_str().map(str::to_owned).unwrap_or_else(|| {
        if value.is_null() {
            String::new()
        } else {
            value.to_string()
        }
    })
}

fn form_choices(schema: &Value) -> Vec<FormChoice> {
    let choices = schema
        .get("enum")
        .and_then(Value::as_array)
        .or_else(|| schema.get("oneOf").and_then(Value::as_array))
        .or_else(|| schema.pointer("/items/anyOf").and_then(Value::as_array))
        .or_else(|| schema.pointer("/items/enum").and_then(Value::as_array));
    choices
        .into_iter()
        .flatten()
        .map(|choice| {
            let value = choice
                .get("const")
                .cloned()
                .unwrap_or_else(|| choice.clone());
            let label = choice
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| input_text(&value));
            FormChoice {
                value,
                label,
                description: choice
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            }
        })
        .collect()
}

fn update_answer(
    mut content: Signal<Map<String, Value>>,
    name: &str,
    update: impl FnOnce(Value) -> Value,
) {
    let previous = content.peek().get(name).cloned().unwrap_or(Value::Null);
    let next = update(previous);
    content.write().insert(name.to_owned(), next);
}

fn toggle_array(current: Value, value: &Value) -> Value {
    let mut values = current.as_array().cloned().unwrap_or_default();
    if let Some(index) = values.iter().position(|entry| entry == value) {
        values.remove(index);
    } else {
        values.push(value.clone());
    }
    json!(values)
}
