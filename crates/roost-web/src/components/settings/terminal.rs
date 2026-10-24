//! Settings → Interface → Terminal: how the terminal behaves on this device.
//!
//! Ports `apps/web/src/components/Settings/TerminalPane.tsx`. Every control is a
//! per-device preference owned by `roost_client_core::store::prefs`; a change
//! arrives as a `ShellIntent` so the store, its persisted copy and the pump's
//! revision all move together, and applies immediately with no reload.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::store::prefs::PredictMode;
use roost_client_core::store::shell_intent::ShellIntent;

use crate::components::settings::color_schemes::TerminalColorSchemes;
use crate::components::term_font_stepper::TermFontStepper;
use crate::pump::use_store;
use crate::term_font_size::{reset_term_font, use_device_default_term_font_px};
use libero::components::{
    Button, Combobox, ComboboxOption, ComboboxOptionArgs, Flex, Paper, Switch, Text, Title,
    use_combobox,
};

fn bell_options() -> Vec<(&'static str, &'static str)> {
    vec![
        ("visual", "Visual"),
        ("sound", "Sound"),
        ("both", "Visual and sound"),
        ("off", "Off"),
    ]
}

fn predict_options() -> Vec<(&'static str, &'static str)> {
    vec![
        (PredictMode::Adaptive.as_str(), "Adaptive (slow links only)"),
        (PredictMode::Always.as_str(), "Always"),
        (
            PredictMode::Experimental.as_str(),
            "Experimental (aggressive)",
        ),
        (PredictMode::Never.as_str(), "Never"),
    ]
}

/// The pane.
#[component]
pub fn TerminalPane() -> Element {
    let pump = use_store();
    let core = pump.core();
    let prefs = core.borrow().store().prefs;
    let default_px = use_device_default_term_font_px();
    let keyboard_pump = pump.clone();
    let mouse_pump = pump.clone();
    let copy_pump = pump.clone();
    let reset_pump = pump.clone();
    let predict_pump = pump.clone();
    let bell_pump = pump.clone();
    let ligatures_pump = pump.clone();
    let predict_picker = use_combobox();
    let bell_picker = use_combobox();
    let predict_options = predict_options();
    let bell_options = bell_options();
    rsx! {
        Flex { direction: "column", gap: "lg",
            "data-testid": "settings-terminal-pane",
            Text { size: "sm", color: "muted",
                "How the terminal behaves on this device. Every setting applies immediately and is saved per device."
            }
            Paper {
                Title { size: "sm", "Soft keyboard" }
                Switch { label: "Resize terminal when the keyboard opens", checked: prefs.keyboard_resize,
                    "data-testid": "keyboard-resize-toggle",
                    onchange: move |on| keyboard_pump.dispatch(ClientEvent::Shell(ShellIntent::SetKeyboardResize { on })) }
            }
            Paper {
                Title { size: "sm", "Mouse mode" }
                Switch { label: "Forward mouse + touch to fullscreen apps", checked: prefs.mouse_forward,
                    "data-testid": "mouse-forward-toggle",
                    onchange: move |on| mouse_pump.dispatch(ClientEvent::Shell(ShellIntent::SetMouseForward { on })) }
            }
            Paper {
                Title { size: "sm", "Selection" }
                Switch { label: "Copy on select", checked: prefs.copy_on_select,
                    "data-testid": "copy-on-select-toggle",
                    onchange: move |on| copy_pump.dispatch(ClientEvent::Shell(ShellIntent::SetCopyOnSelect { on })) }
                Text { size: "sm", color: "muted", "On: releasing a selection in the terminal puts it on the clipboard immediately (the tmux/xterm habit). Off: copy explicitly with the keyboard chord or the right-click menu. Off by default because it overwrites the system clipboard without being asked. This device only." }
            }
            Paper {
                Title { size: "sm", "Text size" }
                Flex { direction: "row", align: "center", gap: "md",
                    TermFontStepper { test_id: "term-font" }
                    Button { variant: "outlined", disabled: prefs.term_font_px == default_px,
                        "data-testid": "term-font-reset",
                        onclick: move |_| reset_term_font(&reset_pump, default_px), "Reset" }
                }
                Text { size: "sm", color: "muted",
                    "The same control sits at the foot of the sidebar, and the keyboard reaches it while a terminal is on screen. Tap the size to return to the default. Changing the text size changes how many columns and rows fit, so every open terminal re-sizes its shell to match. This device only."
                }
            }
            Paper {
                Title { size: "sm", "Local echo" }
                Combobox {
                    state: predict_picker,
                    select_only: true,
                    "aria-label": "Predictive local echo",
                    selected_index: predict_options.iter().position(|(value, _)| *value == prefs.predict.as_str()),
                    option_label: |option: (&'static str, &'static str)| option.1.to_owned(),
                    options: predict_options.clone(),
                    option: {
                        let selected = prefs.predict.as_str();
                        let predict_pump = predict_pump.clone();
                        move |row: ComboboxOptionArgs<(&'static str, &'static str)>| {
                            let predict_pump = predict_pump.clone();
                            rsx! {
                                ComboboxOption {
                                    selected: selected == row.value.0,
                                    onpick: move |_| {
                                        predict_pump.dispatch(ClientEvent::Shell(ShellIntent::SetPredictMode { value: row.value.0.to_owned() }));
                                        predict_picker.close();
                                    },
                                    "{row.value.1}"
                                }
                            }
                        }
                    },
                    Button {
                        variant: "outlined",
                        attributes: predict_picker.a11y_attributes(),
                        "data-testid": "predict-mode-select",
                        "aria-label": "Predictive local echo",
                        onclick: move |_| predict_picker.toggle(),
                        onblur: move |_| predict_picker.close(),
                        "{predict_options.iter().find(|(value, _)| *value == prefs.predict.as_str()).map(|(_, label)| *label).unwrap_or_default()}"
                    }
                }
                Text { size: "sm", color: "muted",
                    "Paints each typed character immediately and reconciles it when the terminal confirms. Adaptive only engages on a high-latency link; Always shows it everywhere except fullscreen apps (for example vim); Experimental shows guesses instantly but may flicker. This device only; applies immediately."
                }
            }
            Paper {
                Title { size: "sm", "Bell" }
                Combobox {
                    state: bell_picker,
                    select_only: true,
                    "aria-label": "Terminal bell",
                    selected_index: bell_options.iter().position(|(value, _)| *value == prefs.terminal_bell.as_str()),
                    option_label: |option: (&'static str, &'static str)| option.1.to_owned(),
                    options: bell_options.clone(),
                    option: {
                        let selected = prefs.terminal_bell.as_str();
                        let bell_pump = bell_pump.clone();
                        move |row: ComboboxOptionArgs<(&'static str, &'static str)>| {
                            let bell_pump = bell_pump.clone();
                            rsx! {
                                ComboboxOption {
                                    selected: selected == row.value.0,
                                    onpick: move |_| {
                                        bell_pump.dispatch(ClientEvent::Shell(ShellIntent::SetTerminalBell { value: row.value.0.to_owned() }));
                                        bell_picker.close();
                                    },
                                    "{row.value.1}"
                                }
                            }
                        }
                    },
                    Button {
                        variant: "outlined",
                        attributes: bell_picker.a11y_attributes(),
                        "data-testid": "terminal-bell-pref",
                        "aria-label": "Terminal bell",
                        onclick: move |_| bell_picker.toggle(),
                        onblur: move |_| bell_picker.close(),
                        "{bell_options.iter().find(|(value, _)| *value == prefs.terminal_bell.as_str()).map(|(_, label)| *label).unwrap_or_default()}"
                    }
                }
                Text { size: "sm", color: "muted",
                    "Visual flashes the terminal briefly. Sound plays a short bell tone. Visual and sound does both. Off disables both; an unseen session still keeps a bell marker until viewed. Reduced-motion devices use a static border pulse."
                }
            }
            Paper {
                Title { size: "sm", "Text shaping" }
                Switch { label: "Font ligatures", checked: prefs.terminal_ligatures,
                    "data-testid": "terminal-ligatures-toggle",
                    onchange: move |on| ligatures_pump.dispatch(ClientEvent::Shell(ShellIntent::SetTerminalLigatures { on })) }
            }
            TerminalColorSchemes {}
        }
    }
}
