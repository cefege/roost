//! Multi-value preference selection and explicit ordering controls.
//! Rows retain the prefs owner ID and setting path for native changes.
//! Selection order is preserved when a value is moved or removed.
use super::super::{event, text_value};
use crate::components::ui::{ButtonSize, ButtonVariant, Checkbox, IconButton, IconType};
use dioxus::prelude::*;
use serde_json::{Value, json};
pub(super) fn multi_control(
    control: &Value,
    prefs_id: &str,
    surface: &str,
    row_id: &str,
    disabled: bool,
    send: EventHandler<Value>,
) -> Element {
    let values = std::rc::Rc::new(
        control
            .get("values")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>(),
    );
    let options = control.get("options").and_then(Value::as_array);
    let ordered = control
        .get("ordered")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let prefs_id = prefs_id.to_owned();
    let surface = surface.to_owned();
    let row_id = row_id.to_owned();
    rsx! {
        div { class: "flex flex-col gap-2",
            if ordered && !values.is_empty() {
                div { class: "flex flex-wrap gap-1",
                    for (idx, value) in values.iter().enumerate() {
                        {
                            let previous = std::rc::Rc::clone(&values);
                            let following = std::rc::Rc::clone(&values);
                            let current = value.clone();
                            let up_id = prefs_id.clone();
                            let up_surface = surface.clone();
                            let up_row = row_id.clone();
                            let down_id = prefs_id.clone();
                            let down_surface = surface.clone();
                            let down_row = row_id.clone();
                            let up_disabled = disabled || idx == 0;
                            let down_disabled = disabled || idx + 1 == values.len();
                            rsx! {
                                div { class: "inline-flex items-center gap-1",
                                    span { class: "text-xs text-muted-foreground", "{current}" }
                                    IconButton {
                                        icon: IconType::ArrowUp,
                                        label: format!("Move {current} up"),
                                        size: ButtonSize::IconXs,
                                        variant: ButtonVariant::Ghost,
                                        disabled: up_disabled,
                                        onclick: move |_| {
                                            if !up_disabled {
                                                let mut next = (*previous).clone();
                                                next.swap(idx, idx - 1);
                                                send.call(event("change", &up_surface, &up_id, json!({"item":up_row,"value":next})));
                                            }
                                        },
                                    }
                                    IconButton {
                                        icon: IconType::ArrowDown,
                                        label: format!("Move {current} down"),
                                        size: ButtonSize::IconXs,
                                        variant: ButtonVariant::Ghost,
                                        disabled: down_disabled,
                                        onclick: move |_| {
                                            if !down_disabled {
                                                let mut next = (*following).clone();
                                                next.swap(idx, idx + 1);
                                                send.call(event("change", &down_surface, &down_id, json!({"item":down_row,"value":next})));
                                            }
                                        },
                                    }
                                }
                            }
                        }
                    }
                }
            }
            div { class: "flex flex-wrap gap-2",
                for option in options.into_iter().flatten() {
                    {
                        let value = option.get("value").and_then(Value::as_str).unwrap_or_default().to_owned();
                        let checked = values.iter().any(|current| current == &value);
                        let label = option.get("label").map(text_value).unwrap_or_else(|| value.clone());
                        let event_id = prefs_id.clone();
                        let sf = surface.clone();
                        let item = row_id.clone();
                        let current_values = std::rc::Rc::clone(&values);
                        rsx! {
                            label { class: "inline-flex items-center gap-2 text-sm",
                                Checkbox {
                                    checked,
                                    disabled,
                                    aria_label: label.clone(),
                                    on_checked_change: move |checked| {
                                        let mut next = current_values.iter().filter(|current| *current != &value).cloned().collect::<Vec<_>>();
                                        if checked { next.push(value.clone()); }
                                        send.call(event("change", &sf, &event_id, json!({"item":item,"value":next})));
                                    },
                                }
                                "{label}"
                            }
                        }
                    }
                }
            }
        }
    }
}
