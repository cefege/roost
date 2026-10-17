//! `Checkbox`: a two-state toggle drawn as a `role="checkbox"` button. The
//! checked state is entirely the caller's: the button has no state of its own,
//! so a click only reports the requested state and the caller re-renders.
//! Settings forms compose it with a `Label` or a `SwitchRow`-style row.

use dioxus::prelude::*;
use tw_merge::tw_merge;

use super::{Icon, IconType};

#[component]
pub fn Checkbox(
    #[props(into, default)] class: Option<String>,
    #[props(default = false)] checked: bool,
    #[props(default = false)] disabled: bool,
    #[props(default, optional)] on_checked_change: Option<EventHandler<bool>>,
    #[props(default = "Checkbox".to_string(), into)] aria_label: String,
    #[props(into, optional)] aria_described_by: Option<String>,
    #[props(into, optional)] test_id: Option<String>,
    /// The checkbox only shows a state its parent control owns (a selectable
    /// card): out of the tab order and the accessibility tree, and inert to the
    /// pointer, so one press never toggles both the box and the parent.
    #[props(default = false)]
    presentational: bool,
) -> Element {
    let checked_state = if checked { "checked" } else { "unchecked" };
    let merged = tw_merge!(
        "peer border-input dark:bg-input/30 data-[state=checked]:bg-primary data-[state=checked]:text-primary-foreground dark:data-[state=checked]:bg-primary data-[state=checked]:border-primary focus-visible:border-ring focus-visible:ring-ring/50 aria-invalid:ring-destructive/20 dark:aria-invalid:ring-destructive/40 aria-invalid:border-destructive size-4 shrink-0 rounded-[4px] border shadow-xs transition-shadow outline-none focus-visible:ring-[3px] disabled:cursor-not-allowed disabled:opacity-50",
        if presentational {
            "pointer-events-none"
        } else {
            ""
        },
        class.as_deref().unwrap_or("")
    );
    rsx! {
        button {
            "data-name": "Checkbox",
            class: "{merged}",
            "data-state": checked_state,
            "data-testid": test_id,
            r#type: "button",
            role: if presentational { None } else { Some("checkbox") },
            "aria-checked": if presentational { None } else { Some(checked.to_string()) },
            "aria-label": if presentational { None } else { Some(aria_label) },
            "aria-describedby": aria_described_by,
            "aria-hidden": presentational.then_some("true"),
            tabindex: presentational.then_some("-1"),
            "data-presentational": presentational.then_some("true"),
            disabled,
            onclick: move |_| {
                if !disabled && let Some(handler) = &on_checked_change {
                    handler.call(!checked);
                }
            },
            span {
                "data-name": "CheckboxIndicator",
                class: "flex justify-center items-center text-current transition-none",
                if checked {
                    Icon { icon: IconType::Check, class: "size-3.5" }
                }
            }
        }
    }
}
