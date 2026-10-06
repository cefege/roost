//! The mic control's presentation: the words on it, and the browser calls that
//! make a tap reach a microphone.
//!
//! Split out of `super::mobile_voice_input` so the component reads as the state
//! machine's only owner and the vocabulary of its own surface — glyph names,
//! captions, the watchdog — sits apart from the effects it performs. Ports the
//! icon/label maps and the iOS-safe open order of
//! `apps/web/src/components/MobileVoiceInput.tsx`.

#[cfg(target_arch = "wasm32")]
use std::rc::Rc;

use dioxus::prelude::*;

use crate::pump::Pump;
use crate::voice::engine::Engine;
use crate::voice::state::VoiceState;

pub fn choose_engine(deepgram_configured: bool) -> Engine {
    #[cfg(target_arch = "wasm32")]
    return Engine::choose(crate::voice::capabilities::probe(deepgram_configured));
    #[cfg(not(target_arch = "wasm32"))]
    return Engine::choose(crate::voice::engine::EngineInputs {
        deepgram_configured,
        ..crate::voice::engine::EngineInputs::default()
    });
}

/// Warm the device and latch the permission, when this build has a browser.
#[cfg(not(target_arch = "wasm32"))]
pub fn warm_mic_for(_engine: Engine) {}

/// The engines this composer owns, built on first use so a composer nobody taps
/// opens no microphone.
#[cfg(target_arch = "wasm32")]
#[derive(Debug, Default)]
pub struct EngineHosts {
    deepgram: Option<Rc<crate::voice::Deepgram>>,
    web_speech: Option<crate::voice::WebSpeech>,
}

#[cfg(target_arch = "wasm32")]
impl EngineHosts {
    #[must_use]
    pub fn new() -> Self {
        Self {
            deepgram: None,
            web_speech: None,
        }
    }

    #[allow(unused_variables)]
    pub fn start(
        &mut self,
        engine: Engine,
        language: String,
        pump: Pump,
        keyterms: crate::voice::KeytermSource,
        sink: crate::voice::EngineSink,
    ) {
        match engine {
            Engine::Deepgram => {
                if self.deepgram.is_none() {
                    self.deepgram = Some(crate::voice::Deepgram::shared(
                        language,
                        crate::voice::grant_source(pump),
                        keyterms,
                        sink,
                    ));
                }
                if let Some(deepgram) = &self.deepgram {
                    deepgram.start();
                }
            }
            Engine::WebSpeech => {
                if self.web_speech.is_none() {
                    self.web_speech = crate::voice::WebSpeech::new(sink);
                }
                if let Some(speech) = &self.web_speech {
                    speech.start();
                }
            }
            Engine::Unavailable => {}
        }
    }

    /// Stop every engine with the intent to insert. Takes `&self` because an
    /// engine with nothing left to wait for settles inside this call, and that
    /// settle re-enters the hosts to clear the engines' text.
    pub fn stop(&self) {
        if let Some(deepgram) = &self.deepgram {
            deepgram.stop();
        }
        if let Some(speech) = &self.web_speech {
            speech.stop();
        }
    }

    pub fn abort(&self) {
        if let Some(deepgram) = &self.deepgram {
            deepgram.abort();
        }
        if let Some(speech) = &self.web_speech {
            speech.abort();
        }
    }

    pub fn reset(&self) {
        if let Some(deepgram) = &self.deepgram {
            deepgram.reset();
        }
        if let Some(speech) = &self.web_speech {
            speech.reset();
        }
    }
}

/// A build without a browser owns no engines, and says so rather than pretending
/// one started.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Default)]
pub struct EngineHosts;

#[cfg(not(target_arch = "wasm32"))]
impl EngineHosts {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    pub fn start(&mut self) {}

    pub fn stop(&self) {}

    pub fn abort(&self) {}

    pub fn reset(&self) {}
}

/// Read the coordinator's stored configuration once per mount.
pub fn read_config(
    pump: Pump,
    #[allow(unused_mut)] mut configured: Signal<bool>,
    #[allow(unused_mut)] mut language: Signal<String>,
) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        use roost_client_core::client::rpc::calls::settings::transcription::GetDictationConfig;
        match pump.rpc().call(&GetDictationConfig).await {
            Ok(config) => {
                configured.set(config.deepgram_configured);
                language.set(config.deepgram_language);
            }
            Err(error) => {
                tracing::warn!(target: "voice", %error, "dictation config read refused");
            }
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, configured, language);
}

/// Whether the page may ask for a microphone at all.
#[allow(unused_variables)]
pub fn is_secure_context() -> bool {
    cfg!(target_arch = "wasm32") && web_sys_window_secure()
}

#[cfg(target_arch = "wasm32")]
pub fn web_sys_window_secure() -> bool {
    crate::voice::capabilities::is_secure_context()
}

#[cfg(not(target_arch = "wasm32"))]
pub fn web_sys_window_secure() -> bool {
    false
}

/// The icon glyph for one state, verbatim the ligature names v2 used.
pub fn mic_icon(state: VoiceState) -> &'static str {
    match state {
        VoiceState::Idle => "mic",
        VoiceState::Starting => "progress_activity",
        VoiceState::Listening => "stop",
        VoiceState::Finalizing => "keyboard_return",
    }
}

/// The control's accessible name for one state.
pub fn mic_label(state: VoiceState) -> &'static str {
    match state {
        VoiceState::Idle => "Start recording",
        VoiceState::Starting => "Starting recording",
        VoiceState::Listening => "Stop and insert",
        VoiceState::Finalizing => "Inserting",
    }
}

pub fn bool_attr(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

pub fn keep_keyboard(event: MouseEvent) {
    event.prevent_default();
}
