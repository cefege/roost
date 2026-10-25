//! `DesignControlStates`: the interactive control reference on `/design` —
//! button variants and sizes, text fields and selects in their default,
//! disabled and invalid states, chips, and the boolean controls. `gallery.rs`
//! mounts it. It composes the shipped `ui` primitives and reimplements none of
//! their semantics: the specimens are controlled by the gallery's own signals.

use dioxus::prelude::*;

use crate::components::ui::{
    Button, ButtonSize, ButtonVariant, Checkbox, Chip, IconButton, IconType, List, ListRow,
    SectionTitle, Select, SelectOption, Switch, SwitchRow, TextField,
};

/// The select specimens' options.
pub fn surface_options() -> Vec<SelectOption> {
    vec![
        SelectOption::new("surface", "Surface"),
        SelectOption::new("accent", "Accent"),
        SelectOption::new("status", "Status"),
    ]
}

const CONTROL_ROW_CLASS: &str = "flex flex-wrap items-center gap-3";
const CONTROL_ROW_SPACED_CLASS: &str = "flex flex-wrap items-center gap-3 mt-3";
const FIELD_GRID_CLASS: &str = "grid grid-cols-[repeat(auto-fit,minmax(16rem,1fr))] gap-4";

/// The control reference.
#[component]
pub fn DesignControlStates() -> Element {
    let mut switch_on = use_signal(|| true);
    let mut checked = use_signal(|| false);
    let mut select_value = use_signal(|| "surface".to_string());
    let mut text_value = use_signal(String::new);
    let mut textarea_value = use_signal(String::new);
    let mut switch_row_on = use_signal(|| true);
    let mut selected_chip = use_signal(|| false);
    let chip_label = if selected_chip() {
        "Interactive selected"
    } else {
        "Interactive action"
    };

    rsx! {
        div { class: "grid gap-6",
            section {
                SectionTitle { "Buttons" }
                div { class: CONTROL_ROW_CLASS,
                    Button { variant: ButtonVariant::Default, "Default" }
                    Button { variant: ButtonVariant::Secondary, "Secondary" }
                    Button { variant: ButtonVariant::Outline, "Outline" }
                    Button { variant: ButtonVariant::Ghost, "Ghost" }
                    Button { variant: ButtonVariant::Destructive, "Destructive" }
                    Button { variant: ButtonVariant::Link, "Link" }
                }
                div { class: CONTROL_ROW_SPACED_CLASS,
                    Button { size: ButtonSize::Sm, "Small" }
                    Button { size: ButtonSize::Default, "Default" }
                    Button { size: ButtonSize::Lg, "Large" }
                    Button {
                        size: ButtonSize::Icon,
                        variant: ButtonVariant::Secondary,
                        icon: IconType::X,
                        "aria-label": "Icon-sized secondary button",
                    }
                    IconButton { size: ButtonSize::IconXs, icon: IconType::X, label: "Extra small icon button" }
                    IconButton { size: ButtonSize::IconSm, icon: IconType::X, label: "Small icon button" }
                    IconButton { size: ButtonSize::Icon, icon: IconType::X, label: "Default icon button" }
                }
            }

            section {
                SectionTitle { "Fields" }
                div { class: FIELD_GRID_CLASS,
                    TextField {
                        value: text_value(),
                        on_input: move |value: String| text_value.set(value),
                        label: "Workspace name",
                        placeholder: "roost",
                        description: rsx! { "A visible field description stays associated with the input." },
                    }
                    TextField {
                        value: textarea_value(),
                        on_input: move |value: String| textarea_value.set(value),
                        label: "Prompt",
                        input_type: "textarea",
                        rows: 4,
                        placeholder: "Describe the task…",
                        description: rsx! { "Textarea fields preserve native multiline editing." },
                    }
                    TextField {
                        value: "Unavailable",
                        on_input: move |_| {},
                        label: "Disabled field",
                        description: rsx! { "Disabled controls retain their context." },
                        disabled: true,
                    }
                    TextField {
                        value: "invalid name",
                        on_input: move |_| {},
                        label: "Invalid field",
                        error: rsx! { "Use lowercase letters, numbers, and hyphens." },
                    }
                    Select {
                        value: select_value(),
                        on_change: move |value: String| select_value.set(value),
                        label: "Surface role",
                        description: rsx! { "The native select opens its shared option surface." },
                        options: surface_options(),
                    }
                    Select {
                        value: "surface",
                        on_change: move |_| {},
                        label: "Disabled select",
                        description: rsx! { "Unavailable controls retain their native disabled semantics." },
                        options: surface_options(),
                        disabled: true,
                    }
                    Select {
                        value: "accent",
                        on_change: move |_| {},
                        label: "Invalid select",
                        options: surface_options(),
                        error: rsx! { "Choose a supported surface role." },
                    }
                }
            }

            section {
                SectionTitle { "Chips" }
                div { class: CONTROL_ROW_CLASS,
                    Chip { label: "Passive label", icon: IconType::Folder }
                    Chip {
                        label: chip_label,
                        icon: IconType::Zap,
                        selected: selected_chip(),
                        onclick: move |_| selected_chip.toggle(),
                    }
                }
            }

            section {
                SectionTitle { "Boolean controls" }
                List {
                    ListRow {
                        headline: rsx! { "Switch" },
                        support: rsx! { "Interactive and controlled by the gallery." },
                        trailing: rsx! {
                            Switch {
                                checked: switch_on(),
                                on_checked_change: move |value: bool| switch_on.set(value),
                                aria_label: "Enable switch specimen",
                            }
                        },
                    }
                    ListRow {
                        headline: rsx! { "Checkbox" },
                        support: rsx! { "Interactive and controlled by the gallery." },
                        trailing: rsx! {
                            Checkbox {
                                checked: checked(),
                                on_checked_change: move |value: bool| checked.set(value),
                                aria_label: "Enable checkbox specimen",
                            }
                        },
                    }
                    ListRow {
                        headline: rsx! { "Disabled switch" },
                        support: rsx! { "Unavailable controls preserve their native disabled semantics." },
                        trailing: rsx! {
                            Switch {
                                checked: false,
                                on_checked_change: move |_| {},
                                aria_label: "Disabled switch specimen",
                                disabled: true,
                            }
                        },
                    }
                    ListRow {
                        headline: rsx! { "Disabled checkbox" },
                        support: rsx! { "Unavailable controls preserve their native disabled semantics." },
                        trailing: rsx! {
                            Checkbox {
                                checked: true,
                                on_checked_change: move |_| {},
                                aria_label: "Disabled checkbox specimen",
                                disabled: true,
                            }
                        },
                    }
                }
                div { class: "mt-4",
                    SectionTitle { "Switch rows" }
                    div { class: "grid gap-4",
                        SwitchRow {
                            headline: "Switch row",
                            support: "A labelled setting keeps its support text associated with the switch.",
                            checked: switch_row_on(),
                            on_checked_change: move |value: bool| switch_row_on.set(value),
                        }
                        SwitchRow {
                            headline: "Disabled switch row",
                            support: "Unavailable settings retain their native disabled semantics.",
                            checked: false,
                            on_checked_change: move |_| {},
                            disabled: true,
                        }
                    }
                }
            }
        }
    }
}
