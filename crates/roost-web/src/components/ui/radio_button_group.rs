//! `RadioButtonGroup`: a segmented control, where each option is a native radio
//! input hidden behind its label. The native inputs keep arrow-key movement and
//! the form `name` grouping; the caller supplies one `name` for the whole group.

use dioxus::prelude::*;
use tw_merge::tw_merge;

const GROUP_CLASS: &str = "flex flex-wrap justify-center mt-2";

#[component]
pub fn RadioButtonText(
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let merged = tw_merge!(
        "block cursor-pointer bg-transparent text-primary px-3 py-1.5 relative ml-px ring-1 ring-border tracking-wider text-center transition-colors duration-500 peer-checked:bg-primary peer-checked:text-primary-foreground peer-focus-visible:ring-ring",
        class.as_deref().unwrap_or("")
    );
    rsx! { span { "data-name": "RadioButtonText", class: "{merged}", {children} } }
}

#[component]
pub fn RadioButtonGroup(#[props(into)] name: String, children: Element) -> Element {
    let inner_class = tw_merge!(
        GROUP_CLASS,
        "[&>label:first-child>span]:rounded-l-md [&>label:last-child>span]:rounded-r-md"
    );
    rsx! {
        fieldset { class: "{GROUP_CLASS}", "data-name": "RadioButtonGroup", "data-group": "{name}",
            div { class: "{inner_class}", role: "radiogroup", {children} }
        }
    }
}

#[component]
pub fn RadioButton(
    #[props(into)] name: String,
    #[props(into)] value: String,
    #[props(default = false)] checked: bool,
    #[props(optional)] onchange: Option<EventHandler<FormEvent>>,
    children: Element,
) -> Element {
    rsx! {
        label { class: "relative",
            input {
                r#type: "radio",
                name,
                value,
                class: "sr-only peer",
                checked,
                onchange: move |event| {
                    if let Some(handler) = &onchange {
                        handler.call(event);
                    }
                },
            }
            {children}
        }
    }
}
