//! A controlled dynamic Combobox specimen using stable profile identities.
//! Availability changes its rows without resetting the selected value.
//! Libero owns active rows, navigation, selection and popup lifetime.

use dioxus::prelude::*;
use libero::components::{
    Button, Checkbox, Combobox, ComboboxOption, ComboboxOptionArgs, Flex, Text, use_combobox,
};

#[derive(Clone, Copy, PartialEq)]
enum Profile {
    Fast,
    Balanced,
    Thorough,
}

fn label(profile: Profile) -> &'static str {
    match profile {
        Profile::Fast => "Fast",
        Profile::Balanced => "Balanced",
        Profile::Thorough => "Thorough",
    }
}

#[component]
pub(super) fn ComboboxSpecimen() -> Element {
    let state = use_combobox();
    let mut selected = use_signal(|| Profile::Balanced);
    let mut thorough_available = use_signal(|| true);
    let options = if thorough_available() {
        vec![Profile::Fast, Profile::Balanced, Profile::Thorough]
    } else {
        vec![Profile::Fast, Profile::Balanced]
    };
    let selected_index = options.iter().position(|profile| *profile == selected());
    rsx! {
        Flex { direction: "column", gap: "sm",
            Combobox {
                state, select_only: true, options, selected_index,
                "aria-label": "Execution profile",
                option_label: |profile: Profile| label(profile).to_owned(),
                option: move |row: ComboboxOptionArgs<Profile>| rsx! {
                    ComboboxOption {
                        selected: selected() == row.value,
                        onpick: move |_| { selected.set(row.value); state.close(); },
                        "{label(row.value)}"
                    }
                },
                Button {
                    variant: "outlined", size: "sm", "aria-label": "Execution profile",
                    attributes: state.a11y_attributes(),
                    onclick: move |_| state.toggle(), onblur: move |_| state.close(),
                    "{label(selected())}"
                }
            }
            Checkbox {
                label: "Thorough profile is available", checked: thorough_available(),
                onchange: move |value| thorough_available.set(value),
            }
            Text { size: "sm", role: "status", "Selected profile: {label(selected())}" }
        }
    }
}
