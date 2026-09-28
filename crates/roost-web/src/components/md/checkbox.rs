//! `Checkbox`: the native checkbox. Ported from
//! `apps/web/src/components/Settings/md/Checkbox.tsx`; settings forms compose it.
//! `controls.css` owns its look; the input keeps browser keyboard, focus and
//! accessibility behaviour.
//!
//! Controlled on the same terms as `Switch`: the element returns to the `checked`
//! the caller rendered after every change, whatever the caller did with it.

use dioxus::prelude::*;

use super::dom::restore_checked;

/// A checkbox.
#[component]
pub fn Checkbox(
    checked: bool,
    on_change: EventHandler<bool>,
    label: String,
    test_id: Option<String>,
    #[props(default)] disabled: bool,
    aria_described_by: Option<String>,
) -> Element {
    rsx! {
        input {
            class: "roost-checkbox",
            r#type: "checkbox",
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
