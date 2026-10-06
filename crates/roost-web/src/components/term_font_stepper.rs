//! `TermFontStepper`: smaller · size · larger for the terminal text size, where
//! tapping the size returns it to this device's default. Mounted vertically in
//! the desktop activity rail, in the compact drawer's text-size row and in the
//! Settings → Terminal pane; every press goes
//! through `crate::term_font_size`, and every mounted pane re-measures from the
//! `--term-font-size` the app root applies.

use dioxus::prelude::*;

use crate::components::md::class_list::class_list;
use crate::components::md::{Button, ButtonSize, ButtonVariant, IconButton, IconButtonSize};
use crate::pump::use_store;
use crate::term_font_size::{
    TermFontStepState, reset_term_font, step_term_font, use_device_default_term_font_px,
};

/// The stepper props.
#[derive(Props, Clone, PartialEq, Debug)]
pub struct TermFontStepperProps {
    /// The `data-testid` stem: the controls are `{stem}-smaller`,
    /// `{stem}-size` and `{stem}-larger`.
    pub test_id: String,
    /// The two step buttons' square size.
    #[props(default)]
    pub size: IconButtonSize,
    /// An extra class on the group.
    pub class: Option<String>,
    /// Stack larger · size · smaller top to bottom, for the narrow rail.
    #[props(default)]
    pub vertical: bool,
}

/// The stepper.
#[component]
pub fn TermFontStepper(props: TermFontStepperProps) -> Element {
    let pump = use_store();
    let px = pump.core().borrow().store().prefs.term_font_px;
    let state = TermFontStepState::at(px, use_device_default_term_font_px());
    let default_px = state.default_px;
    let smaller_pump = pump.clone();
    let larger_pump = pump.clone();
    let reset_pump = pump;
    let stem = props.test_id;
    let reset_label = if state.at_default() {
        format!("Terminal text size {px}px (default)")
    } else {
        format!("Terminal text size {px}px; reset to {default_px}px")
    };
    let value_size = match props.size {
        IconButtonSize::IconXs => ButtonSize::Xs,
        IconButtonSize::IconSm => ButtonSize::Sm,
        IconButtonSize::Icon => ButtonSize::Default,
        IconButtonSize::IconLg => ButtonSize::Lg,
    };
    rsx! {
        div {
            class: class_list([
                "term-font-stepper",
                if props.vertical { "term-font-stepper--vertical" } else { "" },
                props.class.as_deref().unwrap_or(""),
            ]),
            role: "group",
            "aria-label": "Terminal text size",
            if props.vertical {
                IconButton {
                    icon: "text_increase",
                    label: "Larger terminal text",
                    size: props.size,
                    "data-testid": format!("{stem}-larger"),
                    title: "Larger terminal text",
                    disabled: !state.can_grow,
                    onclick: move |_| step_term_font(&larger_pump, 1),
                }
                Button {
                    class: "term-font-stepper__value",
                    variant: ButtonVariant::Ghost,
                    size: value_size,
                    "data-testid": format!("{stem}-size"),
                    title: reset_label.clone(),
                    "aria-label": reset_label,
                    onclick: move |_| reset_term_font(&reset_pump, default_px),
                    {format!("{px}px")}
                }
                IconButton {
                    icon: "text_decrease",
                    label: "Smaller terminal text",
                    size: props.size,
                    "data-testid": format!("{stem}-smaller"),
                    title: "Smaller terminal text",
                    disabled: !state.can_shrink,
                    onclick: move |_| step_term_font(&smaller_pump, -1),
                }
            } else {
                IconButton {
                    icon: "text_decrease",
                    label: "Smaller terminal text",
                    size: props.size,
                    "data-testid": format!("{stem}-smaller"),
                    title: "Smaller terminal text",
                    disabled: !state.can_shrink,
                    onclick: move |_| step_term_font(&smaller_pump, -1),
                }
                Button {
                    class: "term-font-stepper__value",
                    variant: ButtonVariant::Ghost,
                    size: value_size,
                    "data-testid": format!("{stem}-size"),
                    title: reset_label.clone(),
                    "aria-label": reset_label,
                    onclick: move |_| reset_term_font(&reset_pump, default_px),
                    {format!("{px}px")}
                }
                IconButton {
                    icon: "text_increase",
                    label: "Larger terminal text",
                    size: props.size,
                    "data-testid": format!("{stem}-larger"),
                    title: "Larger terminal text",
                    disabled: !state.can_grow,
                    onclick: move |_| step_term_font(&larger_pump, 1),
                }
            }
        }
    }
}
