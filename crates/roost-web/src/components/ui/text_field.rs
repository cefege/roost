//! `TextField`: a labelled native input or textarea with its description and
//! error, the form control the settings, account and rename forms compose. It
//! assembles `Field`, `Input`/`Textarea` and the field text from `field.rs`, so
//! the id and `aria-describedby` rules are the ones every form control uses.
//!
//! The caller keeps the value: every keystroke goes to `on_input`, and the
//! mounted control is handed out through `onmounted`.

use dioxus::prelude::*;
use tw_merge::tw_merge;

use crate::components::ui::field::{
    Field, FieldDescription, FieldError, FieldLabel, described_by, description_id, error_id,
    scoped_element_id,
};
use crate::components::ui::input::{Input, InputType};
use crate::components::ui::textarea::Textarea;

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
    control_class: Option<String>,
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
    role: Option<String>,
    aria_controls: Option<String>,
    aria_expanded: Option<String>,
    aria_active_descendant: Option<String>,
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
    let aria_invalid_attribute = invalid;
    let min = min.map(|min| min.to_string());
    let max = max.map(|max| max.to_string());
    let on_value = move |event: FormEvent| on_input.call(event.value());
    let is_textarea = input_type.as_deref() == Some(TEXTAREA_TYPE);
    let class = tw_merge!("min-w-0 w-full", class.as_deref().unwrap_or(""));
    rsx! {
        Field { class, invalid,
            if let Some(label) = label {
                FieldLabel { html_for: control_id.clone(), {label} }
            }
            if is_textarea {
                Textarea {
                    id: control_id.clone(),
                    class: control_class.unwrap_or_default(),
                    value,
                    rows: i64::from(rows.unwrap_or(4)),
                    placeholder,
                    autocomplete,
                    inputmode: input_mode,
                    required,
                    minlength: min_length,
                    maxlength: max_length,
                    autofocus,
                    disabled,
                    test_id,
                    aria_describedby: control_described_by,
                    aria_invalid: aria_invalid_attribute,
                    aria_label,
                    oninput: on_value,
                    onkeydown: move |event| {
                        if let Some(handler) = &onkeydown {
                            handler.call(event);
                        }
                    },
                    onmounted: move |event| {
                        if let Some(handler) = &onmounted {
                            handler.call(event);
                        }
                    },
                }
            } else {
                Input {
                    id: control_id.clone(),
                    class: control_class.unwrap_or_default(),
                    r#type: InputType::from_name(input_type.as_deref().unwrap_or("text")),
                    value,
                    min,
                    max,
                    placeholder,
                    autocomplete,
                    inputmode: input_mode,
                    required,
                    minlength: min_length,
                    maxlength: max_length,
                    autofocus,
                    disabled,
                    test_id,
                    aria_describedby: control_described_by,
                    aria_invalid: aria_invalid_attribute,
                    aria_label,
                    role,
                    aria_controls,
                    aria_expanded,
                    aria_activedescendant: aria_active_descendant,
                    oninput: on_value,
                    onkeydown: move |event| {
                        if let Some(handler) = &onkeydown {
                            handler.call(event);
                        }
                    },
                    onmounted: move |event| {
                        if let Some(handler) = &onmounted {
                            handler.call(event);
                        }
                    },
                }
            }
            if let Some(description) = description {
                FieldDescription { id: description_element_id, {description} }
            }
            if let Some(error) = error {
                FieldError { id: error_element_id, {error} }
            }
        }
    }
}
