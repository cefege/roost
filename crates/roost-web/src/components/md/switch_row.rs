//! `SwitchRow`: a labelled boolean setting — headline, support line and the
//! switch. Ported from `apps/web/src/components/Settings/md/SwitchRow.tsx`;
//! settings panes own the preference state and supply the copy.
//!
//! The support line is wired to the switch through `aria-describedby`, so a
//! screen reader announces what the setting does, not only its name.

use dioxus::prelude::*;

use super::form_field::scoped_element_id;
use super::switch::Switch;

/// A preference row.
#[component]
pub fn SwitchRow(
    headline: String,
    support: Option<String>,
    checked: bool,
    on_change: EventHandler<bool>,
    test_id: Option<String>,
    #[props(default)] disabled: bool,
) -> Element {
    let support_id = use_hook(|| format!("{}-support", scoped_element_id("roost-switch-row")));
    let described_by = support.is_some().then(|| support_id.clone());
    rsx! {
        div { style: "display: flex; align-items: center; gap: var(--md-space-4);",
            div { style: "flex: 1; min-width: 0;",
                div {
                    class: "md-body-m",
                    style: "color: var(--md-sys-color-on-surface);",
                    {headline.clone()}
                }
                if let Some(support) = support {
                    div {
                        id: support_id,
                        class: "md-body-s",
                        style: "color: var(--md-sys-color-on-surface-variant);",
                        {support}
                    }
                }
            }
            Switch {
                checked,
                on_change,
                label: headline,
                test_id,
                disabled,
                aria_described_by: described_by,
            }
        }
    }
}
