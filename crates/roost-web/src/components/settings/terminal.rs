//! Settings → Interface → Terminal: how the terminal behaves on this device.
//!
//! Ports `apps/web/src/components/Settings/TerminalPane.tsx`. Every control is a
//! per-device preference owned by `roost_client_core::store::prefs`; a change
//! arrives as a `ShellIntent` so the store, its persisted copy and the pump's
//! revision all move together, and applies immediately with no reload.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::store::prefs::PredictMode;
use roost_client_core::store::prefs::terminal_font::{
    TERM_FONT_MAX_PX, TERM_FONT_MIN_PX, TERMINAL_FONT_DEFAULT_PX,
};
use roost_client_core::store::shell_intent::ShellIntent;

use crate::components::md::{
    Button, ButtonVariant, Card, IconButton, Select, SelectOption, SwitchRow,
};
use crate::pump::use_store;

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
    let font_px = prefs.term_font_px;
    let keyboard_pump = pump.clone();
    let mouse_pump = pump.clone();
    let copy_pump = pump.clone();
    let smaller_pump = pump.clone();
    let larger_pump = pump.clone();
    let reset_pump = pump.clone();
    let predict_pump = pump.clone();
    rsx! {
        div {
            class: "settings-pane",
            style: "display: flex; flex-direction: column; gap: var(--md-space-5);",
            "data-testid": "settings-terminal-pane",
            p { class: "md-body-s", style: "color: var(--md-sys-color-on-surface-variant); margin: 0;",
                "How the terminal behaves on this device. Every setting applies immediately and is saved per device."
            }
            Card { title: "Soft keyboard",
                SwitchRow {
                    test_id: "keyboard-resize-toggle",
                    headline: "Resize terminal when the keyboard opens",
                    support: "On: the terminal shrinks to fit above the on-screen keyboard and grows back when it closes. Off: the terminal keeps its size and slides up so the input stays visible (the top scrolls off). This device only.",
                    checked: prefs.keyboard_resize,
                    on_change: move |on| keyboard_pump.dispatch(ClientEvent::Shell(ShellIntent::SetKeyboardResize { on })),
                }
            }
            Card { title: "Mouse mode",
                SwitchRow {
                    test_id: "mouse-forward-toggle",
                    headline: "Forward mouse + touch to fullscreen apps",
                    on_change: move |on| mouse_pump.dispatch(ClientEvent::Shell(ShellIntent::SetMouseForward { on })),
                    checked: prefs.mouse_forward,
                }
            }
            Card { title: "Selection",
                SwitchRow {
                    test_id: "copy-on-select-toggle",
                    headline: "Copy on select",
                    on_change: move |on| copy_pump.dispatch(ClientEvent::Shell(ShellIntent::SetCopyOnSelect { on })),
                    support: "On: releasing a selection in the terminal puts it on the clipboard immediately (the tmux/xterm habit). Off: copy explicitly with the keyboard chord or the right-click menu. Off by default because it overwrites the system clipboard without being asked. This device only.",
                    checked: prefs.copy_on_select,
                }
            }
            Card { title: "Text size",
                div { style: "display: flex; flex-direction: column; gap: var(--md-space-3);",
                    div { style: "display: flex; align-items: center; gap: var(--md-space-3);",
                        IconButton {
                            icon: "remove",
                            label: "Smaller terminal text",
                            "data-testid": "term-font-smaller",
                            disabled: font_px <= TERM_FONT_MIN_PX,
                            onclick: move |_| smaller_pump.dispatch(ClientEvent::Shell(ShellIntent::StepTermFont { delta: -1 })),
                        }
                        span { class: "md-body-m", "data-testid": "term-font-size",
                            style: "min-width: var(--md-space-9); text-align: center; color: var(--md-sys-color-on-surface);",
                            {format!("{font_px}px")}
                        }
                        IconButton {
                            icon: "add",
                            label: "Larger terminal text",
                            "data-testid": "term-font-larger",
                            disabled: font_px >= TERM_FONT_MAX_PX,
                            onclick: move |_| larger_pump.dispatch(ClientEvent::Shell(ShellIntent::StepTermFont { delta: 1 })),
                        }
                        Button {
                            variant: ButtonVariant::Ghost,
                            "data-testid": "term-font-reset",
                            onclick: move |_| reset_pump.dispatch(ClientEvent::Shell(ShellIntent::ResetTermFont {
                                default_px: TERMINAL_FONT_DEFAULT_PX,
                            })),
                            "Reset"
                        }
                    }
                    p { class: "md-body-s", style: "color: var(--md-sys-color-on-surface-variant); margin: 0;",
                        "The same change is reachable from the keyboard while a terminal is on screen. Changing the text size changes how many columns and rows fit, so every open terminal re-sizes its shell to match. This device only."
                    }
                }
            }
            Card { title: "Local echo",
                div { style: "display: flex; flex-direction: column; gap: var(--md-space-3);",
                    Select {
                        test_id: "predict-mode-select",
                        label: "Predictive local echo",
                        value: prefs.predict.as_str().to_owned(),
                        options: predict_options(),
                        on_change: move |value| {
                            predict_pump.dispatch(ClientEvent::Shell(ShellIntent::SetPredictMode { value }))
                        },
                    }
                    p { class: "md-body-s", style: "color: var(--md-sys-color-on-surface-variant); margin: 0;",
                        "Paints each typed character immediately and reconciles it when the terminal confirms. Adaptive only engages on a high-latency link; Always shows it everywhere except fullscreen apps (for example vim); Experimental shows guesses instantly but may flicker. This device only; applies immediately."
                    }
                }
            }
        }
    }
}
