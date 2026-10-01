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
use crate::components::md::{
    Button, ButtonVariant, Card, Icon, IconSize, Select, StatusDot, SwitchRow, TextField,
};
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
    let provider_status = if configured { "ok" } else { "warn" };
    rsx! {
        div {
            class: "settings-pane",
            style: "display: flex; flex-direction: column; gap: var(--md-space-5);",
            "data-testid": "settings-transcription-pane",
            Card { class: "settings-hero", test_id: "transcription-status",
                div { style: "display: flex; align-items: center; gap: var(--md-space-4);",
                    Icon { name: "mic", filled: true, size: IconSize::Lg }
                    div { style: "flex: 1; min-width: 0;",
                        div { style: "display: flex; align-items: center; gap: var(--md-space-2);",
                            StatusDot { status: provider_status }
                            span { class: "md-title-m", {provider_name} }
                        }
                        div { class: "md-body-s", style: "color: var(--md-sys-color-on-surface-variant);",
                            if configured {
                                "Live Deepgram transcription for your authenticated owner devices."
                            } else {
                                "Browser's built-in speech — add a Deepgram key below to upgrade."
                            }
                        }
                    }
                }
            }
            Card { title: "Dictation",
                SwitchRow {
                    test_id: "keyterm-biasing-toggle",
                    headline: "Bias dictation to on-screen terms",
                    support: "Feeds the terminal's visible text, your recent commands, and learned project jargon to Deepgram as keyterms — so names like Kysely, tailnet, or coordFactory transcribe correctly. Turn off to A/B against plain transcription. This device only; next recording.",
                    checked: keyterm_biasing,
                    on_change: move |on| bias_pump.dispatch(ClientEvent::Shell(ShellIntent::SetKeytermBiasing { on })),
                }
            }
            Card {
                title: "API key",
                supporting: "Stored on the coordinator. For direct Deepgram dictation, Roost returns the configured key to this authenticated admin browser, which connects to Deepgram directly. Leave it empty to use the browser's built-in speech.",
                div { style: "display: flex; flex-direction: column; gap: var(--md-space-4);",
                    TextField {
                        test_id: "transcription-deepgram-key",
                        label: "Deepgram API key",
                        input_type: Some("password".to_owned()),
                        placeholder: key_placeholder,
                        value: state.key.clone(),
                        on_input: move |value| view.write().key = value,
                    }
                    Select {
                        test_id: "transcription-language",
                        label: "Language",
                        value: language,
                        options: language_options(),
                        on_change: move |value| view.write().language = value,
                    }
                    div { style: "display: flex; align-items: center; flex-wrap: wrap; gap: var(--md-space-3);",
                        Button {
                            variant: ButtonVariant::Default,
                            "data-testid": "transcription-save",
                            disabled: state.saving,
                            onclick: move |_| save(save_pump.clone(), view),
                            if state.saving { "Saving…" } else { "Save" }
                        }
                        Button {
                            variant: ButtonVariant::Secondary,
                            icon: "check_circle",
                            "data-testid": "transcription-test",
                            disabled: state.testing || !configured,
                            onclick: move |_| test(test_pump.clone(), view),
                            if state.testing { "Testing…" } else { "Test" }
                        }
                        if configured {
                            Button {
                                variant: ButtonVariant::Ghost,
                                "data-testid": "transcription-clear",
                                disabled: state.saving,
                                onclick: move |_| clear(clear_pump.clone(), view),
                                "Remove key"
                            }
                        }
                        if state.saved {
                            span { class: "md-body-s", "data-testid": "transcription-saved",
                                style: "color: var(--md-sys-color-primary);", "Saved"
                            }
                        }
                    }
                    if let Some(message) = state.test_result.clone() {
                        span { class: "md-body-s", "data-testid": "transcription-test-result",
                            style: if state.test_ok { "color: var(--status-ok);" } else { "color: var(--md-sys-color-error);" },
                            {message}
                        }
                    }
                    if let Some(message) = refusal {
                        p { role: "alert", class: "md-body-s", style: "color: var(--md-sys-color-error);", {message} }
                    }
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
