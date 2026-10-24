//! Settings → Interface → Voice: dictation's provider and behaviour.
//!
//! Ports `apps/web/src/components/Settings/TranscriptionPane.tsx`; the language
//! list is `voice_languages`, the Deepgram matrix this pane selects from.
//! Depends on `roost-client-core`'s `TranscriptionGetConfig`/
//! `TranscriptionSetConfig`/`TranscriptionTest` calls.
//!
//! The key is WRITE-ONLY in this surface: the coordinator returns the mask, and
//! a save sends the typed key or nothing. `Some("")` clears it, `None` leaves
//! it alone — the proto's own absent/present split, which a pane that always
//! sent an empty string would turn into a silent credential deletion.

use dioxus::prelude::*;
use libero::components::{
    Button, Combobox, ComboboxOption, ComboboxOptionArgs, Flex, Paper, Pictogram, Switch, Text,
    TextField, Title, use_combobox,
};
use roost_client_core::ClientEvent;
use roost_client_core::client::rpc::calls::settings::transcription::DictationConfig;
use roost_client_core::store::shell_intent::ShellIntent;
// The round trips are browser-only: a native build has no coordinator to ask,
// so the call types are gated with them. The keyterm switch's client-side
// dispatch is not a coordinator answer, so it keeps its types.
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::settings::transcription::{
    GetDictationConfig, SetDictationConfig, TestDictationKey,
};

use super::voice_languages::{language_options, stored_or_default};
use crate::pump::{Pump, use_store};

/// What the coordinator holds, plus what this reader is in the middle of doing.
#[derive(Debug, Clone, PartialEq, Default)]
struct VoiceView {
    config: DictationConfig,
    loaded: bool,
    load_error: Option<String>,
    key: String,
    language: String,
    saving: bool,
    save_error: Option<String>,
    saved: bool,
    testing: bool,
    test_result: Option<String>,
    test_ok: bool,
}

/// The pane.
#[component]
pub fn VoicePane() -> Element {
    let pump = use_store();
    let mut view = use_signal(VoiceView::default);
    let language_picker = use_combobox();
    let core = pump.core();
    let keyterm_biasing = core.borrow().store().prefs.keyterm_biasing;

    let load_pump = pump.clone();
    let bias_pump = pump.clone();
    let save_pump = pump.clone();
    let test_pump = pump.clone();
    let clear_pump = pump.clone();
    use_effect(move || load(load_pump.clone(), view));

    let state = view();
    let configured = state.config.deepgram_configured;
    let key_placeholder = if state.config.deepgram_key_masked.is_empty() {
        "paste API key".to_owned()
    } else {
        state.config.deepgram_key_masked.clone()
    };
    let language = if state.language.is_empty() {
        "en".to_owned()
    } else {
        state.language.clone()
    };
    let refusal = state
        .load_error
        .clone()
        .or_else(|| state.save_error.clone());
    let provider_name = if configured {
        "Deepgram"
    } else {
        "Browser speech"
    };
    let medium_text_sx = use_hook(|| libero::sx::sx().font_weight("500"));
    let language_options = language_options();
    rsx! {
        Flex { direction: "column", gap: "lg",
            "data-testid": "settings-transcription-pane",
            Paper { "data-testid": "transcription-status",
                Flex { direction: "row", align: "center", gap: "md",
                    Pictogram { icon: pictogram_icons_lucide::mic::outlined }
                    Flex { direction: "column", gap: "xs",
                        Text { size: "md", sx: medium_text_sx.clone(), "{provider_name}" }
                        Text { size: "sm", color: "muted",
                            if configured { "Live Deepgram transcription for your authenticated owner devices." }
                            else { "Browser's built-in speech — add a Deepgram key below to upgrade." }
                        }
                    }
                }
            }
            Paper {
                Title { size: "sm", "Dictation" }
                Flex { direction: "row", align: "center", justify: "space-between",
                    Flex { direction: "column", gap: "xs",
                        Text { size: "sm", sx: medium_text_sx.clone(), "Bias dictation to on-screen terms" }
                        Text { size: "sm", color: "muted", "Feeds the terminal's visible text, your recent commands, and learned project jargon to Deepgram as keyterms — so names like Kysely, tailnet, or coordFactory transcribe correctly. Turn off to A/B against plain transcription. This device only; next recording." }
                    }
                    Switch { label: "Bias dictation to on-screen terms", checked: keyterm_biasing,
                        "data-testid": "keyterm-biasing-toggle",
                        onchange: move |on| bias_pump.dispatch(ClientEvent::Shell(ShellIntent::SetKeytermBiasing { on })) }
                }
            }
            Paper {
                Title { size: "sm", "API key" }
                Text { size: "sm", color: "muted", "Stored on the coordinator. For direct Deepgram dictation, Roost returns the configured key to this authenticated admin browser, which connects to Deepgram directly. Leave it empty to use the browser's built-in speech." }
                TextField { label: "Deepgram API key", r#type: "password", placeholder: key_placeholder,
                    value: state.key.clone(), oninput: move |value: String| view.write().key = value,
                    "data-testid": "transcription-deepgram-key" }
                Combobox {
                    state: language_picker,
                    select_only: true,
                    "aria-label": "Language",
                    selected_index: language_options.iter().position(|(code, _)| *code == language),
                    option_label: |option: (String, String)| option.1,
                    options: language_options.clone(),
                    option: {
                        let selected = language.clone();
                        move |row: ComboboxOptionArgs<(String, String)>| rsx! {
                            ComboboxOption {
                                selected: selected == row.value.0,
                                onpick: move |_| {
                                    view.write().language = row.value.0.clone();
                                    language_picker.close();
                                },
                                "{row.value.1}"
                            }
                        }
                    },
                    Button {
                        variant: "outlined",
                        attributes: language_picker.a11y_attributes(),
                        "data-testid": "transcription-language",
                        "aria-label": "Language",
                        onclick: move |_| language_picker.toggle(),
                        onblur: move |_| language_picker.close(),
                        "{language_options.iter().find(|(code, _)| *code == language).map(|(_, label)| label.as_str()).unwrap_or(\"English\")}"
                    }
                }
                Flex { direction: "row", align: "center", gap: "sm",
                    Button { variant: "filled", disabled: state.saving, "data-testid": "transcription-save",
                        onclick: move |_| save(save_pump.clone(), view),
                        if state.saving { "Saving…" } else { "Save" } }
                    Button { variant: "outlined", disabled: state.testing || !configured,
                        "data-testid": "transcription-test",
                        onclick: move |_| test(test_pump.clone(), view),
                        if state.testing { "Testing…" } else { "Test" } }
                    if configured {
                        Button { variant: "text", disabled: state.saving, "data-testid": "transcription-clear",
                            onclick: move |_| clear(clear_pump.clone(), view), "Remove key" }
                    }
                    if state.saved {
                        Text { size: "sm", color: "success", "data-testid": "transcription-saved", "Saved" }
                    }
                }
                if let Some(message) = state.test_result.clone() {
                    Text { color: if state.test_ok { "success" } else { "error" }, role: "status",
                        "data-testid": "transcription-test-result", "{message}" }
                }
                if let Some(message) = refusal {
                    Text { color: "error", role: "alert", "{message}" }
                }
            }
        }
    }
}

/// Read the stored configuration, and seed the language from it once.
fn load(pump: Pump, view: Signal<VoiceView>) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut view = view;
        match pump.rpc().call(&GetDictationConfig).await {
            Ok(config) => {
                let mut view = view.write();
                view.language = stored_or_default(&config.deepgram_language);
                view.config = config;
                view.loaded = true;
            }
            Err(error) => {
                tracing::warn!(target: "settings", %error, "dictation config read refused");
                let mut view = view.write();
                view.loaded = true;
                view.load_error = Some(error.to_string());
            }
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, view);
}

/// Save the language, and the key only when one was typed.
fn save(pump: Pump, mut view: Signal<VoiceView>) {
    let typed = view().key.trim().to_owned();
    let language = stored_or_default(&view().language);
    view.write().saving = true;
    write(pump, view, (!typed.is_empty()).then_some(typed), language);
}

/// Remove the stored key without touching the language.
fn clear(pump: Pump, mut view: Signal<VoiceView>) {
    let language = stored_or_default(&view().language);
    view.write().saving = true;
    write(pump, view, Some(String::new()), language);
}

/// One `TranscriptionSetConfig` write, whatever the reader asked for.
fn write(pump: Pump, view: Signal<VoiceView>, deepgram_key: Option<String>, language: String) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut view = view;
        let request = SetDictationConfig {
            deepgram_key,
            deepgram_language: language,
        };
        match pump.rpc().call(&request).await {
            Ok(config) => {
                let mut view = view.write();
                view.config = config;
                view.key.clear();
                view.saving = false;
                view.save_error = None;
                view.saved = true;
            }
            Err(error) => {
                tracing::warn!(target: "settings", %error, "dictation config write refused");
                let mut view = view.write();
                view.saving = false;
                view.saved = false;
                view.save_error = Some(error.to_string());
            }
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, view, deepgram_key, language);
}

/// Ask the coordinator to use the stored key once.
fn test(pump: Pump, mut view: Signal<VoiceView>) {
    view.write().testing = true;
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        match pump.rpc().call(&TestDictationKey).await {
            Ok(error) => {
                let mut view = view.write();
                view.testing = false;
                if error.is_empty() {
                    view.test_ok = true;
                    view.test_result = Some("Deepgram key works".to_owned());
                } else {
                    view.test_ok = false;
                    view.test_result = Some(error);
                }
            }
            Err(error) => {
                let mut view = view.write();
                view.testing = false;
                view.test_ok = false;
                view.test_result = Some(error.to_string());
            }
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, view);
}
