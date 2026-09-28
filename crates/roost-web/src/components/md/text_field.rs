//! `TextField`: the native text input or textarea with its label, description
//! and error. Ported from `apps/web/src/components/Settings/md/TextField.tsx`;
//! settings, account and rename forms compose it. `controls.css` owns its look.
//!
//! The wrapper owns only layout hooks and `data-invalid`; the control keeps
//! native input semantics, and the caller keeps the value (every keystroke goes
//! to `on_input`) and receives the mounted control where v2 handed out a `ref`.

use dioxus::prelude::*;

use super::class_list::class_list;
use super::form_field::{described_by, description_id, error_id, scoped_element_id};

/// The `input_type` that renders a `<textarea>` instead of an `<input>`.
pub const TEXTAREA_TYPE: &str = "textarea";

/// Whether the field is invalid: the caller said so, or it carries an error.
pub fn text_field_invalid(aria_invalid: bool, has_error: bool) -> bool {
    aria_invalid || has_error
}

/// A text field.
#[component]
pub fn TextField(
    value: String,
    on_input: EventHandler<String>,
    label: Option<String>,
    /// The input type; `"textarea"` renders a multi-line control.
    input_type: Option<String>,
    placeholder: Option<String>,
    class: Option<String>,
    style: Option<String>,
    control_style: Option<String>,
    test_id: Option<String>,
    rows: Option<u32>,
    min: Option<f64>,
    max: Option<f64>,
    autocomplete: Option<String>,
    input_mode: Option<String>,
    #[props(default)] required: bool,
    min_length: Option<u32>,
    max_length: Option<u32>,
    aria_described_by: Option<String>,
    #[props(default)] autofocus: bool,
    onkeydown: Option<EventHandler<KeyboardEvent>>,
    #[props(default)] disabled: bool,
    aria_label: Option<String>,
    onmounted: Option<EventHandler<MountedEvent>>,
    id: Option<String>,
    description: Option<Element>,
    error: Option<Element>,
    #[props(default)] aria_invalid: bool,
) -> Element {
    let generated_id = use_hook(|| scoped_element_id("roost-text-field"));
    let control_id = id.unwrap_or(generated_id);
    let description_element_id = description_id(&control_id);
    let error_element_id = error_id(&control_id);
    let invalid = text_field_invalid(aria_invalid, error.is_some());
    let control_described_by = described_by(
        aria_described_by.as_deref(),
        description
            .is_some()
            .then_some(description_element_id.as_str()),
        error.is_some().then_some(error_element_id.as_str()),
    );
    let aria_invalid_attribute = invalid.then_some("true");
    let min = min.map(|min| min.to_string());
    let max = max.map(|max| max.to_string());
    let on_key_down = move |event: KeyboardEvent| {
        if let Some(handler) = onkeydown {
            handler.call(event);
        }
    };
    let on_mounted = move |event: MountedEvent| {
        if let Some(handler) = onmounted {
            handler.call(event);
        }
    };
    let on_value = move |event: FormEvent| on_input.call(event.value());
    let is_textarea = input_type.as_deref() == Some(TEXTAREA_TYPE);
    rsx! {
        div {
            class: class_list(["roost-text-field", class.as_deref().unwrap_or("")]),
            style,
            "data-invalid": aria_invalid_attribute,
            if let Some(label) = label {
                label { class: "roost-text-field__label", r#for: control_id.clone(), {label} }
            }
            if is_textarea {
                textarea {
                    id: control_id.clone(),
                    class: "roost-text-field__control",
                    style: control_style.clone(),
                    value: value.clone(),
                    rows: rows.map(|rows| rows.to_string()),
                    placeholder: placeholder.clone(),
                    autocomplete: autocomplete.clone(),
                    inputmode: input_mode.clone(),
                    required,
                    minlength: min_length.map(|length| length.to_string()),
                    maxlength: max_length.map(|length| length.to_string()),
                    autofocus,
                    disabled,
                    "data-testid": test_id.clone(),
                    "aria-describedby": control_described_by.clone(),
                    "aria-invalid": aria_invalid_attribute,
                    "aria-label": aria_label.clone(),
                    onkeydown: on_key_down,
                    oninput: on_value,
                    onmounted: on_mounted,
                }
            } else {
                input {
                    id: control_id.clone(),
                    class: "roost-text-field__control",
                    style: control_style,
                    r#type: input_type.unwrap_or_else(|| "text".to_string()),
                    value,
                    min,
                    max,
                    placeholder,
                    autocomplete,
                    inputmode: input_mode,
                    required,
                    minlength: min_length.map(|length| length.to_string()),
                    maxlength: max_length.map(|length| length.to_string()),
                    autofocus,
                    disabled,
                    "data-testid": test_id,
                    "aria-describedby": control_described_by,
                    "aria-invalid": aria_invalid_attribute,
                    "aria-label": aria_label,
                    onkeydown: on_key_down,
                    oninput: on_value,
                    onmounted: on_mounted,
                }
            }
            if let Some(description) = description {
                div { id: description_element_id, class: "roost-text-field__description", {description} }
            }
            if let Some(error) = error {
                div { id: error_element_id, class: "roost-text-field__error", {error} }
            }
        }
    }
}
