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

/// The bell delivery preferences, in their displayed order.
fn bell_options() -> Vec<SelectOption> {
    vec![
        SelectOption::new("visual", "Visual"),
        SelectOption::new("sound", "Sound"),
        SelectOption::new("both", "Visual and sound"),
        SelectOption::new("off", "Off"),
    ]
}

/// The four predictive-echo modes, in the reader's words.
fn predict_options() -> Vec<SelectOption> {
    vec![
        SelectOption::new(PredictMode::Adaptive.as_str(), "Adaptive (slow links only)"),
        SelectOption::new(PredictMode::Always.as_str(), "Always"),
        SelectOption::new(
            PredictMode::Experimental.as_str(),
            "Experimental (aggressive)",
        ),
        SelectOption::new(PredictMode::Never.as_str(), "Never"),
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
    rsx! {
        div {
            class: "flex flex-col gap-5",
            "data-testid": "settings-terminal-pane",
            p { class: "m-0 text-sm text-muted-foreground",
                "How the terminal behaves on this device. Every setting applies immediately and is saved per device."
            }
            Card {
                CardHeader { CardTitle { "Soft keyboard" } }
                CardContent { class: "flex flex-col gap-3",
                    SwitchRow {
                        test_id: Some("keyboard-resize-toggle".to_owned()),
                        headline: "Resize terminal when the keyboard opens".to_owned(),
                        support: Some("On: the terminal shrinks to fit above the on-screen keyboard and grows back when it closes. Off: the terminal keeps its size and slides up so the input stays visible (the top scrolls off). This device only.".to_owned()),
                        checked: prefs.keyboard_resize,
                        on_checked_change: move |on| keyboard_pump.dispatch(ClientEvent::Shell(ShellIntent::SetKeyboardResize { on })),
                    }
                }
            }
            Card {
                CardHeader { CardTitle { "Mouse mode" } }
                CardContent { class: "flex flex-col gap-3",
                    SwitchRow {
                        test_id: Some("mouse-forward-toggle".to_owned()),
                        headline: "Forward mouse + touch to fullscreen apps".to_owned(),
                        on_checked_change: move |on| mouse_pump.dispatch(ClientEvent::Shell(ShellIntent::SetMouseForward { on })),
                        checked: prefs.mouse_forward,
                    }
                }
            }
            Card {
                CardHeader { CardTitle { "Selection" } }
                CardContent { class: "flex flex-col gap-3",
                    SwitchRow {
                        test_id: Some("copy-on-select-toggle".to_owned()),
                        headline: "Copy on select".to_owned(),
                        on_checked_change: move |on| copy_pump.dispatch(ClientEvent::Shell(ShellIntent::SetCopyOnSelect { on })),
                        support: Some("On: releasing a selection in the terminal puts it on the clipboard immediately (the tmux/xterm habit). Off: copy explicitly with the keyboard chord or the right-click menu. Off by default because it overwrites the system clipboard without being asked. This device only.".to_owned()),
                        checked: prefs.copy_on_select,
                    }
                }
            }
            Card {
                CardHeader { CardTitle { "Text size" } }
                CardContent { class: "flex flex-col gap-3",
                    div { class: "flex items-center gap-3",
                        TermFontStepper { test_id: "term-font" }
                        Button {
                            variant: ButtonVariant::Ghost,
                            "data-testid": "term-font-reset",
                            disabled: prefs.term_font_px == default_px,
                            onclick: move |_| reset_term_font(&reset_pump, default_px),
                            "Reset"
                        }
                    }
                    p { class: "m-0 text-sm text-muted-foreground",
                        "The same control sits at the foot of the sidebar, and the keyboard reaches it while a terminal is on screen. Tap the size to return to the default. Changing the text size changes how many columns and rows fit, so every open terminal re-sizes its shell to match. This device only."
                    }
                }
            }
            Card {
                CardHeader { CardTitle { "Local echo" } }
                CardContent { class: "flex flex-col gap-3",
                    Select {
                        test_id: Some("predict-mode-select".to_owned()),
                        label: Some("Predictive local echo".to_owned()),
                        value: prefs.predict.as_str().to_owned(),
                        options: predict_options(),
                        on_change: move |value| {
                            predict_pump.dispatch(ClientEvent::Shell(ShellIntent::SetPredictMode { value }))
                        },
                    }
                    p { class: "m-0 text-sm text-muted-foreground",
                        "Paints each typed character immediately and reconciles it when the terminal confirms. Adaptive only engages on a high-latency link; Always shows it everywhere except fullscreen apps (for example vim); Experimental shows guesses instantly but may flicker. This device only; applies immediately."
                    }
                }
            }
            Card {
                CardHeader { CardTitle { "Bell" } }
                CardContent { class: "flex flex-col gap-3",
                    Select {
                        test_id: Some("terminal-bell-pref".to_owned()),
                        label: Some("Terminal bell".to_owned()),
                        value: prefs.terminal_bell.as_str().to_owned(),
                        options: bell_options(),
                        on_change: move |value| bell_pump.dispatch(ClientEvent::Shell(
                            ShellIntent::SetTerminalBell { value }
                        )),
                    }
                    p { class: "m-0 text-sm text-muted-foreground",
                        "Visual flashes the terminal briefly. Sound plays a short bell tone. Visual and sound does both. Off disables both; an unseen session still keeps a bell marker until viewed. Reduced-motion devices use a static border pulse."
                    }
                }
            }
            Card {
                CardHeader { CardTitle { "Text shaping" } }
                CardContent { class: "flex flex-col gap-3",
                    SwitchRow {
                        test_id: Some("terminal-ligatures-toggle".to_owned()),
                        headline: "Font ligatures".to_owned(),
                        support: Some("Allow programming-font ligatures across adjacent terminal cells with matching style. Grid columns remain fixed. This device only.".to_owned()),
                        checked: prefs.terminal_ligatures,
                        on_checked_change: move |on| ligatures_pump.dispatch(ClientEvent::Shell(
                            ShellIntent::SetTerminalLigatures { on }
                        )),
                    }
                }
            }
            TerminalColorSchemes {}
        }
    }
}
