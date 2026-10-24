//! Interactive examples of Libero controls used by Roost.

use dioxus::prelude::*;
use libero::{
    components::{
        ActionIcon, Button, Checkbox, FieldStatus, Flex, RadioGroup, SegmentedControl, Slider,
        SliderChangeEvent, Switch, Text, TextField, Textarea, Title,
    },
    sx::sx,
};

use super::combobox_specimen::ComboboxSpecimen;

#[component]
pub fn DesignControlStates() -> Element {
    let mut count = use_signal(|| 0_u32);
    let mut name = use_signal(String::new);
    let mut prompt = use_signal(String::new);
    let mut checked = use_signal(|| true);
    let mut enabled = use_signal(|| false);
    let mut radio = use_signal(|| "fleet".to_string());
    let mut segment = use_signal(|| "comfortable".to_string());
    let mut amount = use_signal(|| 42.0_f64);
    let field_sx = use_hook(|| sx().width("100%"));

    rsx! {
        Title { component: "h2", size: "lg", "Actions and controls" }
        Flex { direction: "row", wrap: true, gap: "sm", align: "center",
            Button { size: "sm", onclick: move |_| count += 1, "Click specimen ({count})" }
            ActionIcon { size: "sm", icon: pictogram_icons_lucide::rotate_ccw::outlined,
                aria_label: "Reset button count", onclick: move |_| count.set(0)
            }
            Button { size: "sm", variant: "outlined", disabled: true, "Unavailable" }
            Button { size: "sm", loading: true, disabled: true, "Saving" }
        }
        TextField {
            sx: field_sx.clone(),
            label: "Workspace name",
            description: "A controlled field updates as you type.",
            placeholder: "roost",
            value: name(),
            oninput: move |value| name.set(value),
            status: if name().contains(' ') { FieldStatus::Error("Names cannot contain spaces.".into()) } else { FieldStatus::Valid },
        }
        Textarea {
            sx: field_sx,
            label: "Task prompt",
            description: "Multiline native editing.",
            placeholder: "Describe the task…",
            value: prompt(),
            oninput: move |value| prompt.set(value),
            rows: 3,
        }
        Checkbox { checked: checked(), onchange: move |value: bool| checked.set(value), label: "Remember this choice" }
        Switch { checked: enabled(), onchange: move |value: bool| enabled.set(value), label: "Enable live updates" }
        RadioGroup {
            options: vec!["fleet".to_string(), "local".to_string()],
            option_label: |value: String| if value == "fleet" { "Fleet".to_string() } else { "Local".to_string() },
            value: Some(radio()),
            onchange: move |value: String| radio.set(value),
            label: "Connection mode",
        }
        SegmentedControl {
            options: vec!["comfortable".to_string(), "compact".to_string()],
            option_label: |value: String| -> libero::components::OptionLabel { if value == "comfortable" { "Comfortable".into() } else { "Compact".into() } },
            value: segment(),
            onchange: move |value: String| segment.set(value),
        }
        Slider {
            value: amount(),
            min: 0.0,
            max: 100.0,
            aria_label: "Resource limit",
            oninput: move |event: SliderChangeEvent| amount.set(event.value()),
        }
        ComboboxSpecimen {}
        Text { role: "status", "Name: {name}; density: {segment}; resource limit: {amount:.0}%" }
    }
}
