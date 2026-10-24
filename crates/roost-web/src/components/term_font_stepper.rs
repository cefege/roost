//! `TermFontStepper`: smaller · size · larger for the terminal text size, where
//! tapping the size returns it to this device's default. Mounted vertically in
//! the desktop activity rail, in the compact drawer's text-size row and in the
//! Settings → Terminal pane; every press goes
//! through `crate::term_font_size`, and every mounted pane re-measures from the
//! `--term-font-size` the app root applies.

use crate::pump::use_store;
use crate::term_font_size::{
    TermFontStepState, reset_term_font, step_term_font, use_device_default_term_font_px,
};
use dioxus::prelude::*;
use libero::{
    components::{ActionIcon, Button, Flex, Pictogram},
    sx::sx,
};

#[derive(Props, Clone, PartialEq, Debug)]
pub struct TermFontStepperProps {
    /// The `data-testid` stem: the controls are `{stem}-smaller`,
    /// `{stem}-size` and `{stem}-larger`.
    pub test_id: String,
    /// Libero's action-icon size.
    #[props(default = "sm".to_owned())]
    pub size: String,
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
    let row_sx = use_hook(|| sx().gap("xs").align_items("center"));
    let value_sx = use_hook(|| sx().min_width("3rem").padding_inline("xs"));
    let (
        first_pump,
        second_pump,
        first_label,
        second_label,
        first_icon,
        second_icon,
        first_test,
        second_test,
        first_disabled,
        second_disabled,
        first_step,
        second_step,
    ) = if props.vertical {
        (
            larger_pump,
            smaller_pump,
            "Larger terminal text",
            "Smaller terminal text",
            pictogram_icons_lucide::a_arrow_up::outlined,
            pictogram_icons_lucide::a_arrow_down::outlined,
            format!("{stem}-larger"),
            format!("{stem}-smaller"),
            !state.can_grow,
            !state.can_shrink,
            1,
            -1,
        )
    } else {
        (
            smaller_pump,
            larger_pump,
            "Smaller terminal text",
            "Larger terminal text",
            pictogram_icons_lucide::a_arrow_down::outlined,
            pictogram_icons_lucide::a_arrow_up::outlined,
            format!("{stem}-smaller"),
            format!("{stem}-larger"),
            !state.can_shrink,
            !state.can_grow,
            -1,
            1,
        )
    };
    let direction = if props.vertical { "column" } else { "row" };
    rsx! {
        Flex {
            sx: row_sx,
            direction,
            wrap: false,
            role: "group",
            aria_label: "Terminal text size",
            if props.vertical {
                ActionIcon {
                    aria_label: first_label,
                    size: props.size.clone(),
                    "data-testid": first_test,
                    title: first_label,
                    disabled: first_disabled,
                    onclick: move |_| step_term_font(&first_pump, first_step),
                    Pictogram { icon: first_icon, width: "1rem", height: "1rem" }
                }
                Button {
                    sx: value_sx.clone(), variant: "standard", size: "xs",
                    "data-testid": format!("{stem}-size"),
                    title: reset_label.clone(),
                    aria_label: reset_label.clone(),
                    onclick: move |_| reset_term_font(&reset_pump, default_px),
                    "{px}px"
                }
                ActionIcon {
                    aria_label: second_label,
                    size: props.size,
                    "data-testid": second_test,
                    title: second_label,
                    disabled: second_disabled,
                    onclick: move |_| step_term_font(&second_pump, second_step),
                    Pictogram { icon: second_icon, width: "1rem", height: "1rem" }
                }
            } else {
                ActionIcon {
                    aria_label: first_label,
                    size: props.size.clone(),
                    "data-testid": first_test,
                    title: first_label,
                    disabled: first_disabled,
                    onclick: move |_| step_term_font(&first_pump, first_step),
                    Pictogram { icon: first_icon, width: "1rem", height: "1rem" }
                }
                Button {
                    sx: value_sx, variant: "standard", size: "xs",
                    "data-testid": format!("{stem}-size"),
                    title: reset_label.clone(),
                    aria_label: reset_label,
                    onclick: move |_| reset_term_font(&reset_pump, default_px),
                    "{px}px"
                }
                ActionIcon {
                    aria_label: second_label,
                    size: props.size,
                    "data-testid": second_test,
                    title: second_label,
                    disabled: second_disabled,
                    onclick: move |_| step_term_font(&second_pump, second_step),
                    Pictogram { icon: second_icon, width: "1rem", height: "1rem" }
                }
            }
        }
    }
}
