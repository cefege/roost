//! The browser's own recognizer: the fallback engine.
//!
//! `web-sys` has no `SpeechRecognition` binding — the API is not in the crate's
//! WebIDL surface — so this drives it through `js_sys::Reflect`. What the engine
//! contract promises is preserved: a start, a stop that settles, an abort, and
//! interim plus final transcripts. The engine name in `data-engine` is the
//! string the oracles select on, so it comes from the same choice function as
//! Deepgram's.
//! Ports the Web Speech branch of `apps/web/src/components/MobileVoiceInput.tsx`.

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};

use super::deepgram_engine::{EngineEvent, EngineSink};
use super::handshake::captions;

/// The language the recognizer is configured with. v2 fixed it at `en-US`: a
/// stored Deepgram key is the way to change the language, and a tag the browser
/// has no voice for would leave the operator with silence and no explanation.
pub const RECOGNITION_LANGUAGE: &str = "en-US";

/// What the handler closures can reach: the sink, and the transcript so far.
///
/// Separate from the recognizer object so the closures can own a handle to this
/// without a cycle back through the object whose properties they are.
struct SpeechState {
    sink: EngineSink,
    /// Finalized words, in the order the recognizer returned them.
    settled: RefCell<String>,
    /// The recognizer's current guess, which the next result may replace.
    hypothesis: RefCell<String>,
}

impl SpeechState {
    fn publish(&self) {
        (self.sink)(EngineEvent::Transcript {
            settled: self.settled.borrow().clone(),
            hypothesis: self.hypothesis.borrow().clone(),
        });
    }

    fn clear(&self) {
        self.settled.borrow_mut().clear();
        self.hypothesis.borrow_mut().clear();
    }
}

/// One recording's recognizer.
pub struct WebSpeech {
    recognizer: Object,
    state: Rc<SpeechState>,
    /// The handler closures, owned here so the recognizer cannot outlive them: a
    /// dropped handler property is a dropped callback.
    handlers: RefCell<Vec<Rc<Closure<dyn FnMut(JsValue)>>>>,
}

impl std::fmt::Debug for WebSpeech {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("WebSpeech").finish_non_exhaustive()
    }
}

impl WebSpeech {
    /// Construct a recognizer, or `None` when this browser has none.
    #[must_use]
    pub fn new(sink: EngineSink) -> Option<Self> {
        let constructor = constructor()?;
        // `new`, not a call: the recognizer is an ES class in every engine that
        // has one, and calling a class without `new` throws — which a stand-in
        // in a test harness is too.
        let recognizer = Reflect::construct(&constructor, &Array::new()).ok()?;
        let recognizer: Object = recognizer.into();
        let speech = Self {
            recognizer,
            state: Rc::new(SpeechState {
                sink,
                settled: RefCell::new(String::new()),
                hypothesis: RefCell::new(String::new()),
            }),
            handlers: RefCell::new(Vec::new()),
        };
        speech.configure();
        speech.bind();
        Some(speech)
    }

    /// `continuous` and `interimResults` are what make a dictation one utterance
    /// instead of one phrase.
    fn configure(&self) {
        for (name, value) in [
            ("continuous", JsValue::TRUE),
            ("interimResults", JsValue::TRUE),
            ("lang", JsValue::from_str(RECOGNITION_LANGUAGE)),
        ] {
            let _ = Reflect::set(self.recognizer.as_ref(), &JsValue::from_str(name), &value);
        }
    }

    fn bind(&self) {
        self.on("onstart", |state: &SpeechState, _event| {
            (state.sink)(EngineEvent::Live);
        });
        self.on("onresult", |state: &SpeechState, event| {
            read_results(state, event);
        });
        self.on("onend", |state: &SpeechState, _event| {
            state.publish();
            (state.sink)(EngineEvent::Settled);
        });
        self.on("onerror", |state: &SpeechState, _event| {
            state.clear();
            (state.sink)(EngineEvent::Failed(captions::ATTACH_REFUSED.to_owned()));
        });
    }

    /// Attach one handler property, keeping the closure for the recognizer's own
    /// lifetime.
    fn on(&self, name: &str, visit: impl Fn(&SpeechState, &JsValue) + 'static) {
        let state = Rc::clone(&self.state);
        let handler = Closure::wrap(
            Box::new(move |event: JsValue| visit(&state, &event)) as Box<dyn FnMut(JsValue)>
        );
        let _ = Reflect::set(
            self.recognizer.as_ref(),
            &JsValue::from_str(name),
            handler.as_ref().unchecked_ref(),
        );
        let dropped = Rc::new(handler);
        self.handlers.borrow_mut().push(dropped);
    }

    /// Begin listening.
    pub fn start(&self) {
        self.call("start");
    }

    /// Stop, keeping what was heard.
    pub fn stop(&self) {
        self.call("stop");
    }

    /// Stop, keeping nothing.
    pub fn abort(&self) {
        self.call("abort");
    }

    /// Forget the transcript without touching the recognizer.
    pub fn reset(&self) {
        self.state.clear();
        self.state.publish();
    }

    fn call(&self, name: &str) {
        if let Ok(method) = Reflect::get(self.recognizer.as_ref(), &JsValue::from_str(name))
            && let Ok(method) = method.dyn_into::<Function>()
        {
            let _ = method.call0(self.recognizer.as_ref());
        }
    }
}

/// Read every result the event carries: the finalized ones are kept, the
/// trailing non-final one is the hypothesis.
fn read_results(state: &SpeechState, event: &JsValue) {
    let Some(results) = event_results(event) else {
        return;
    };
    let mut settled: Vec<String> = Vec::new();
    let mut hypothesis = String::new();
    for index in 0..results.length() {
        let Ok(result) = Reflect::get(&results, &JsValue::from_f64(f64::from(index))) else {
            continue;
        };
        let transcript = Reflect::get(&result, &JsValue::from_str("0"))
            .ok()
            .and_then(|alternative| {
                Reflect::get(&alternative, &JsValue::from_str("transcript")).ok()
            })
            .and_then(|value| value.as_string())
            .unwrap_or_default();
        if transcript.trim().is_empty() {
            continue;
        }
        let is_final = Reflect::get(&result, &JsValue::from_str("isFinal"))
            .ok()
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        if is_final {
            settled.push(transcript);
        } else {
            hypothesis = transcript;
        }
    }
    *state.settled.borrow_mut() = settled.join(" ");
    *state.hypothesis.borrow_mut() = hypothesis;
    state.publish();
}

/// The result list off one recognition event.
fn event_results(event: &JsValue) -> Option<Array> {
    let results = Reflect::get(event, &JsValue::from_str("results")).ok()?;
    results.dyn_into().ok()
}

/// The `SpeechRecognition` constructor, prefixed where Safari still needs it.
#[must_use]
pub fn constructor() -> Option<Function> {
    for name in ["SpeechRecognition", "webkitSpeechRecognition"] {
        if let Ok(value) = Reflect::get(&js_sys::global(), &JsValue::from_str(name))
            && let Ok(function) = value.dyn_into::<Function>()
        {
            return Some(function);
        }
    }
    None
}
