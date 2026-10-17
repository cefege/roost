//! `Switch`: a boolean toggle drawn as a `role="switch"` button, and
//! `SwitchRow`, the labelled settings row built on it. Both are controlled: the
//! checked state belongs to the caller, so a rejected change leaves the switch
//! showing the value the caller rendered.

use dioxus::prelude::*;
use tw_merge::tw_merge;

use crate::components::ui::field::scoped_element_id;

#[component]
pub fn SwitchLabel(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged = tw_merge!("text-sm font-medium", class.as_deref().unwrap_or(""));
    rsx! { span { class: "{merged}", {children} } }
}

#[component]
pub fn Switch(
    #[props(into, optional)] id: Option<String>,
    #[props(default = false)] checked: bool,
    #[props(default = false)] disabled: bool,
    #[props(default, optional)] on_checked_change: Option<EventHandler<bool>>,
    #[props(default = "Toggle switch".to_string(), into)] aria_label: String,
    #[props(into, optional)] aria_described_by: Option<String>,
    #[props(into, optional)] test_id: Option<String>,
    #[props(into, optional)] class: Option<String>,
) -> Element {
    let state = if checked { "checked" } else { "unchecked" };
    let track_class = tw_merge!(
        "inline-flex h-6 w-11 shrink-0 cursor-pointer items-center rounded-full border-2 border-transparent transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background disabled:cursor-not-allowed disabled:opacity-50 data-[state=checked]:bg-primary data-[state=unchecked]:bg-input",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        button {
            "data-name": "Switch",
            id,
            r#type: "button",
            role: "switch",
            aria_checked: "{checked}",
            aria_label: "{aria_label}",
            "aria-describedby": aria_described_by,
            "data-testid": test_id,
            "data-state": "{state}",
            class: "{track_class}",
            disabled,
            onclick: move |_| {
                if !disabled && let Some(handler) = &on_checked_change {
                    handler.call(!checked);
                }
            },
            span {
                "data-state": "{state}",
                class: "block rounded-full ring-0 shadow-lg transition-transform pointer-events-none size-5 bg-background data-[state=checked]:translate-x-5 data-[state=unchecked]:translate-x-0",
            }
        }
    }
}

/// A labelled boolean setting: headline, optional support line, and the switch.
/// The support line is the switch's `aria-describedby`, so a screen reader
/// announces what the setting does, not only its name.
#[component]
pub fn SwitchRow(
    headline: String,
    support: Option<String>,
    checked: bool,
    on_checked_change: EventHandler<bool>,
    test_id: Option<String>,
    #[props(default)] disabled: bool,
) -> Element {
    let support_id = use_hook(|| format!("{}-support", scoped_element_id("roost-switch-row")));
    let described_by = support.is_some().then(|| support_id.clone());
    rsx! {
        div { class: "flex items-center gap-4",
            div { class: "flex-1 min-w-0",
                div { class: "text-sm font-medium text-foreground", {headline.clone()} }
                if let Some(support) = support {
                    div {
                        id: support_id,
                        class: "text-sm font-normal text-muted-foreground",
                        {support}
                    }
                }
            }
            Switch {
                checked,
                disabled,
                aria_label: headline,
                test_id,
                aria_described_by: described_by,
                on_checked_change,
            }
        }
    }
}
