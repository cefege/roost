//! `Input`: the native single-line `<input>` with rust-ui's field styling. The
//! props carry the ARIA, keyboard and mount wiring the form and combobox
//! controls need; the caller keeps the value and receives every change.

use dioxus::prelude::*;
use tw_merge::tw_merge;

/// The native `type` of an input.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputType {
    #[default]
    Text,
    Email,
    Password,
    Number,
    Tel,
    Url,
    Search,
    Time,
    DatetimeLocal,
    Date,
    Month,
    Week,
    Hidden,
    File,
    Checkbox,
    Radio,
    Color,
    Range,
}

impl InputType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Email => "email",
            Self::Password => "password",
            Self::Number => "number",
            Self::Tel => "tel",
            Self::Url => "url",
            Self::Search => "search",
            Self::Time => "time",
            Self::DatetimeLocal => "datetime-local",
            Self::Date => "date",
            Self::Month => "month",
            Self::Week => "week",
            Self::Hidden => "hidden",
            Self::File => "file",
            Self::Checkbox => "checkbox",
            Self::Radio => "radio",
            Self::Color => "color",
            Self::Range => "range",
        }
    }

    /// The type a caller names by its HTML spelling. Names outside this set
    /// are plain text, which is also what the browser does with them.
    #[must_use]
    pub fn from_name(name: &str) -> Self {
        match name {
            "email" => Self::Email,
            "password" => Self::Password,
            "number" => Self::Number,
            "tel" => Self::Tel,
            "url" => Self::Url,
            "search" => Self::Search,
            "time" => Self::Time,
            "datetime-local" => Self::DatetimeLocal,
            "date" => Self::Date,
            "month" => Self::Month,
            "week" => Self::Week,
            _ => Self::Text,
        }
    }
}

#[component]
pub fn Input(
    #[props(into, optional)] class: Option<String>,
    #[props(default = InputType::default())] r#type: InputType,
    #[props(into, optional)] placeholder: Option<String>,
    #[props(into, optional)] name: Option<String>,
    #[props(into, optional)] id: Option<String>,
    #[props(into, optional)] value: Option<String>,
    #[props(default = false)] disabled: bool,
    #[props(default = false)] readonly: bool,
    #[props(default = false)] required: bool,
    #[props(default = false)] autofocus: bool,
    #[props(default = false)] aria_invalid: bool,
    #[props(into, optional)] autocomplete: Option<String>,
    #[props(into, optional)] inputmode: Option<String>,
    #[props(into, optional)] min: Option<String>,
    #[props(into, optional)] max: Option<String>,
    #[props(optional)] minlength: Option<u32>,
    #[props(optional)] maxlength: Option<u32>,
    #[props(into, optional)] aria_label: Option<String>,
    #[props(into, optional)] aria_describedby: Option<String>,
    #[props(into, optional)] aria_controls: Option<String>,
    #[props(into, optional)] aria_expanded: Option<String>,
    #[props(into, optional)] aria_activedescendant: Option<String>,
    #[props(into, optional)] role: Option<String>,
    #[props(into, optional)] test_id: Option<String>,
    /// The file kinds a `File` input offers, in HTML's `accept` syntax.
    #[props(into, optional)] accept: Option<String>,
    #[props(optional)] oninput: Option<EventHandler<FormEvent>>,
    #[props(optional)] onchange: Option<EventHandler<FormEvent>>,
    #[props(optional)] onkeydown: Option<EventHandler<KeyboardEvent>>,
    #[props(optional)] onmounted: Option<EventHandler<MountedEvent>>,
) -> Element {
    let merged_class = tw_merge!(
        "text-foreground file:text-foreground placeholder:text-muted-foreground selection:bg-primary selection:text-primary-foreground dark:bg-input/30 border-input flex h-9 w-full min-w-0 rounded-md border bg-transparent px-3 py-1 text-base shadow-xs transition-[color,box-shadow] outline-none file:inline-flex file:h-7 file:border-0 file:bg-transparent file:text-sm file:font-medium disabled:pointer-events-none disabled:cursor-not-allowed disabled:opacity-50 md:text-sm",
        "focus-visible:border-ring focus-visible:ring-ring/50",
        "focus-visible:ring-2",
        "aria-invalid:ring-destructive/20 dark:aria-invalid:ring-destructive/40 aria-invalid:border-destructive",
        "read-only:bg-muted",
        class.as_deref().unwrap_or("")
    );
    let aria_invalid_attribute = aria_invalid.then_some("true");
    rsx! {
        input {
            "data-name": "Input",
            r#type: r#type.as_str(),
            class: "{merged_class}",
            placeholder,
            value,
            name,
            id,
            disabled,
            readonly,
            required,
            autofocus,
            autocomplete,
            inputmode,
            min,
            max,
            minlength: minlength.map(|length| length.to_string()),
            maxlength: maxlength.map(|length| length.to_string()),
            "aria-label": aria_label,
            "aria-describedby": aria_describedby,
            "aria-invalid": aria_invalid_attribute,
            "aria-controls": aria_controls,
            "aria-expanded": aria_expanded,
            "aria-activedescendant": aria_activedescendant,
            role,
            "data-testid": test_id,
            accept,
            oninput: move |event| {
                if let Some(handler) = &oninput {
                    handler.call(event);
                }
            },
            onchange: move |event| {
                if let Some(handler) = &onchange {
                    handler.call(event);
                }
            },
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
}
