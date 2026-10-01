//! Which dictation engine this browser will use, decided from what the
//! coordinator stores and what the browser exposes.
//!
//! Split out from the component because the answer is a contract the Playwright
//! oracles read off the DOM (`data-engine`), and because it is the one decision
//! that must be right before a microphone is opened: a browser with neither
//! engine must say so, not open a device and record silence.
//! Ports the engine selection of `apps/web/src/components/MobileVoiceInput.tsx`
//! (`useDeepgram`, `webSupported`, `deepgramSupported`, `engineAvailable`).

/// What the browser exposes. Probed by `super::capabilities`; passed in here so
/// the decision itself needs no browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EngineInputs {
    /// The coordinator has a Deepgram key stored, so the browser can be handed
    /// one and open the Deepgram socket itself.
    pub deepgram_configured: bool,
    /// `SpeechRecognition` or `webkitSpeechRecognition` is on the window.
    pub web_speech_supported: bool,
    /// The Deepgram transport is usable: `WebSocket`, `getUserMedia` and an
    /// `AudioContext` are all present.
    pub deepgram_supported: bool,
}

/// The engine one browser recording runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    /// Neither engine is present; the mic cannot start at all.
    Unavailable,
    /// The coordinator's Deepgram key, over the browser's own socket.
    Deepgram,
    /// The browser's built-in recognizer.
    WebSpeech,
}

impl Engine {
    /// Choose the engine.
    ///
    /// Deepgram wins when it is both configured AND transportable: a stored key
    /// with no microphone API is not a working engine, and silently falling to
    /// the browser recognizer would hide a configuration problem behind a
    /// different transcription path.
    #[must_use]
    pub fn choose(inputs: EngineInputs) -> Self {
        if !inputs.web_speech_supported && !inputs.deepgram_supported {
            return Self::Unavailable;
        }
        if inputs.deepgram_configured && inputs.deepgram_supported {
            return Self::Deepgram;
        }
        Self::WebSpeech
    }

    /// Whether a recording could start at all.
    #[must_use]
    pub fn is_available(&self) -> bool {
        !matches!(self, Self::Unavailable)
    }

    /// The `data-engine` value. Verbatim the string v2 emits, because the specs
    /// select on it.
    #[must_use]
    pub fn data_engine(&self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Deepgram => "deepgram",
            Self::WebSpeech => "web-speech",
        }
    }
}
/// Why a tap on an idle mic did nothing, and the caption that says so.
///
/// The distinction the operator needs is "this page cannot ask for a microphone"
/// versus "this browser has no microphone input", so the secure-context check
/// is asked first.
#[must_use]
pub fn start_refusal(engine: Engine, secure_context: bool) -> Option<&'static str> {
    if engine.is_available() {
        return None;
    }
    if !secure_context {
        Some("Dictation needs an https connection to this page.")
    } else {
        Some("This browser has no microphone input.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deepgram_wins_when_it_is_configured_and_transportable() {
        let engine = Engine::choose(EngineInputs {
            deepgram_configured: true,
            web_speech_supported: true,
            deepgram_supported: true,
        });
        assert_eq!(engine, Engine::Deepgram);
        assert_eq!(engine.data_engine(), "deepgram");
    }

    #[test]
    fn no_stored_key_selects_the_browser_recognizer() {
        let engine = Engine::choose(EngineInputs {
            deepgram_configured: false,
            web_speech_supported: true,
            deepgram_supported: true,
        });
        assert_eq!(engine, Engine::WebSpeech);
        assert_eq!(engine.data_engine(), "web-speech");
    }

    #[test]
    fn a_stored_key_without_a_mic_api_does_not_select_deepgram() {
        let engine = Engine::choose(EngineInputs {
            deepgram_configured: true,
            web_speech_supported: true,
            deepgram_supported: false,
        });
        assert_eq!(engine, Engine::WebSpeech);
    }

    #[test]
    fn neither_engine_is_unavailable_whatever_the_key() {
        let engine = Engine::choose(EngineInputs {
            deepgram_configured: true,
            web_speech_supported: false,
            deepgram_supported: false,
        });
        assert_eq!(engine, Engine::Unavailable);
        assert_eq!(engine.data_engine(), "unavailable");
        assert!(!engine.is_available());
        assert!(!Engine::choose(EngineInputs::default()).is_available());
    }

    #[test]
    fn the_refusal_names_the_https_page_before_the_missing_microphone() {
        assert_eq!(
            start_refusal(Engine::Unavailable, false),
            Some("Dictation needs an https connection to this page.")
        );
        assert_eq!(
            start_refusal(Engine::Unavailable, true),
            Some("This browser has no microphone input.")
        );
    }

    #[test]
    fn a_working_engine_never_refuses_a_tap() {
        for engine in [Engine::Deepgram, Engine::WebSpeech] {
            assert_eq!(start_refusal(engine, false), None);
            assert_eq!(start_refusal(engine, true), None);
        }
    }
}
