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
use crate::components::ui::{
    Button, ButtonVariant, Card, CardContent, CardHeader, CardTitle, Select, SelectOption,
    SwitchRow,
};
use crate::pump::use_store;
use crate::term_font_size::{reset_term_font, use_device_default_term_font_px};

fn bell_options() -> Vec<SelectOption> {
    ["visual", "sound", "both", "off"]
        .into_iter()
        .map(|value| SelectOption::new(value, value))
        .collect()
}

fn predict_options() -> Vec<SelectOption> {
    [
        PredictMode::Adaptive,
        PredictMode::Always,
        PredictMode::Experimental,
        PredictMode::Never,
    ]
    .into_iter()
    .map(|mode| SelectOption::new(mode.as_str(), mode.as_str()))
    .collect()
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
    rsx! {
        div { class: "flex flex-col gap-5",
            "data-testid": "settings-terminal-pane",
            p { class: "text-sm text-muted-foreground",
                "How the terminal behaves on this device. Every setting applies immediately and is saved per device."
            }
            Card {
                CardHeader { CardTitle { "Soft keyboard" } }
                CardContent {
                    SwitchRow {
                        headline: "Resize terminal when the keyboard opens",
                        checked: prefs.keyboard_resize,
                        test_id: "keyboard-resize-toggle",
                        on_checked_change: move |on| keyboard_pump.dispatch(ClientEvent::Shell(ShellIntent::SetKeyboardResize { on })),
                    }
                }
            }
            Card {
                CardHeader { CardTitle { "Mouse mode" } }
                CardContent {
                    SwitchRow {
                        headline: "Forward mouse + touch to fullscreen apps",
                        checked: prefs.mouse_forward,
                        test_id: "mouse-forward-toggle",
                        on_checked_change: move |on| mouse_pump.dispatch(ClientEvent::Shell(ShellIntent::SetMouseForward { on })),
                    }
                }
            }
            Card {
                CardHeader { CardTitle { "Selection" } }
                CardContent {
                    SwitchRow {
                        headline: "Copy on select",
                        checked: prefs.copy_on_select,
                        test_id: "copy-on-select-toggle",
                        on_checked_change: move |on| copy_pump.dispatch(ClientEvent::Shell(ShellIntent::SetCopyOnSelect { on })),
                        support: "On: releasing a selection in the terminal puts it on the clipboard immediately (the tmux/xterm habit). Off: copy explicitly with the keyboard chord or the right-click menu. Off by default because it overwrites the system clipboard without being asked. This device only.",
                    }
                }
            }
            Card {
                CardHeader { CardTitle { "Text size" } }
                CardContent { class: "flex flex-col gap-3",
                    div { class: "flex flex-wrap items-center gap-3",
                        TermFontStepper { test_id: "term-font" }
                        Button {
                            variant: ButtonVariant::Outline,
                            disabled: prefs.term_font_px == default_px,
                            "data-testid": "term-font-reset",
                            onclick: move |_| reset_term_font(&reset_pump, default_px),
                            "Reset"
                        }
                    }
                    p { class: "text-sm text-muted-foreground",
                        "The same control sits at the foot of the sidebar, and the keyboard reaches it while a terminal is on screen. Tap the size to return to the default. Changing the text size changes how many columns and rows fit, so every open terminal re-sizes its shell to match. This device only."
                    }
                }
            }
            Card {
                CardHeader { CardTitle { "Local echo" } }
                CardContent { class: "flex flex-col gap-3",
                    Select {
                        value: prefs.predict.as_str().to_owned(),
                        options: predict_options(),
                        label: "Predictive local echo",
                        test_id: "predict-mode-select",
                        on_change: move |value| predict_pump.dispatch(ClientEvent::Shell(ShellIntent::SetPredictMode { value })),
                    }
                    p { class: "text-sm text-muted-foreground",
                        "Paints each typed character immediately and reconciles it when the terminal confirms. Adaptive only engages on a high-latency link; Always shows it everywhere except fullscreen apps (for example vim); Experimental shows guesses instantly but may flicker. This device only; applies immediately."
                    }
                }
            }
            Card {
                CardHeader { CardTitle { "Bell" } }
                CardContent { class: "flex flex-col gap-3",
                    Select {
                        value: prefs.terminal_bell.as_str().to_owned(),
                        options: bell_options(),
                        label: "Terminal bell",
                        test_id: "terminal-bell-pref",
                        on_change: move |value| bell_pump.dispatch(ClientEvent::Shell(ShellIntent::SetTerminalBell { value })),
                    }
                    p { class: "text-sm text-muted-foreground",
                        "Visual flashes the terminal briefly. Sound plays a short bell tone. Visual and sound does both. Off disables both; an unseen session still keeps a bell marker until viewed. Reduced-motion devices use a static border pulse."
                    }
                }
            }
            Card {
                CardHeader { CardTitle { "Text shaping" } }
                CardContent {
                    SwitchRow {
                        headline: "Font ligatures",
                        checked: prefs.terminal_ligatures,
                        test_id: "terminal-ligatures-toggle",
                        on_checked_change: move |on| ligatures_pump.dispatch(ClientEvent::Shell(ShellIntent::SetTerminalLigatures { on })),
                    }
                }
            }
            TerminalColorSchemes {}
        }
    }
}
