//! `Switch`: the native boolean switch — a checkbox input with `role="switch"`.
//! Ported from `apps/web/src/components/Settings/md/Switch.tsx`; `SwitchRow` and
//! the settings panes compose it. `controls.css` owns its look.
//!
//! IT STAYS CONTROLLED even when the caller ignores or rejects the change: the
//! requested value goes to `on_change`, and the element is put back to the
//! `checked` the caller rendered. A switch that showed "on" after a rejected
//! save is a setting the reader believes changed.

use dioxus::prelude::*;

use super::dom::restore_checked;

/// A switch.
#[component]
pub fn Switch(
    checked: bool,
    on_change: EventHandler<bool>,
    label: String,
    test_id: Option<String>,
    #[props(default)] disabled: bool,
    aria_described_by: Option<String>,
) -> Element {
    rsx! {
        input {
            class: "roost-switch",
            r#type: "checkbox",
            role: "switch",
            checked,
            disabled,
            "data-testid": test_id,
            "aria-label": label,
            "aria-describedby": aria_described_by,
            onchange: move |event: FormEvent| {
                on_change.call(event.checked());
                restore_checked(&event, checked);
            },
        }
    }
}
