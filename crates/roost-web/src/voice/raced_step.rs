//! One browser promise raced against its deadline, with both failures kept apart.
//!
//! Split out of `super::audio_capture` because it is the one piece of the open
//! that is not about the microphone: `super::capture_open` races the device
//! open and `super::audio_graph` the worklet load through it.

use js_sys::Promise;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

/// How a raced step failed.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum RacedFailure {
    /// The step's own promise rejected, with the browser's reason.
    Refused(JsValue),
    /// The deadline passed first.
    Stalled(&'static str),
}

impl RacedFailure {
    /// The open outcome this failure is, so the caption is chosen in one place.
    pub(super) fn outcome(&self) -> super::handshake::MicOpenOutcome {
        match self {
            Self::Refused(error) => {
                let read = |key: &str| {
                    js_sys::Reflect::get(error, &JsValue::from_str(key))
                        .ok()
                        .and_then(|value| value.as_string())
                        .unwrap_or_default()
                };
                super::handshake::MicOpenOutcome::Refused {
                    name: read("name"),
                    message: read("message"),
                }
            }
            Self::Stalled(step) => super::handshake::MicOpenOutcome::Stalled(step),
        }
    }

    /// The one-line reason a step other than the device open reports.
    ///
    /// The fields are read reflectively rather than `Debug`-formatted, because
    /// this string is CAPTION: it is painted for the operator. `JsValue`'s
    /// `Debug` renders the WRAPPER ("JsValue(Error: denied)") — a debugging
    /// aid for a terminal, not a sentence — so formatting it here would put a
    /// type name on screen. The step is the last resort so a rejection that
    /// carries no readable reason still says which step failed.
    pub(super) fn caption(&self) -> String {
        match self {
            Self::Refused(error) => {
                let read = |key: &str| {
                    js_sys::Reflect::get(error, &JsValue::from_str(key))
                        .ok()
                        .and_then(|value| value.as_string())
                        .filter(|value| !value.is_empty())
                };
                read("message")
                    .or_else(|| read("name"))
                    .unwrap_or_else(|| format!("{} was refused", self.step()))
            }
            Self::Stalled(step) => super::handshake::pipeline_timeout(step),
        }
    }

    /// Which step this failure belongs to, for a rejection that named nothing.
    fn step(&self) -> &'static str {
        match self {
            Self::Refused(_) => "the audio pipeline",
            Self::Stalled(step) => step,
        }
    }
}

/// Await a promise, keeping BOTH of the ways it can fail apart: its own
/// rejection, and the deadline passing first.
///
/// A rejection is carried through untouched, because the browser's reason is
/// the whole diagnosis. A step that only ever attached a success handler would
/// swallow it and answer the deadline caption instead, which is how a
/// refused microphone came to be reported as one that never answered.
pub(super) async fn raced(
    promise: Promise,
    millis: i32,
    step: &'static str,
) -> Result<JsValue, RacedFailure> {
    let outcome = Promise::new(&mut |resolve, reject| {
        if let Some(window) = web_sys::window() {
            let on_expiry = reject.clone();
            let expiry = Closure::once(move || {
                let _ = on_expiry.call1(&JsValue::NULL, &tag(1.0, &JsValue::UNDEFINED));
            });
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                expiry.as_ref().unchecked_ref(),
                millis,
            );
            // The timer outlives this scope by design: the deadline has to fire
            // after the await has been handed back, and a dropped `Closure` is
            // freed memory a pending timer would then call into.
            expiry.forget();
        }
        let settled = Closure::once(move |value: JsValue| {
            let _ = resolve.call1(&JsValue::UNDEFINED, &value);
        });
        let on_refused = reject.clone();
        let refused = Closure::once(move |error: JsValue| {
            let _ = on_refused.call1(&JsValue::NULL, &tag(0.0, &error));
        });
        // Three closures, three owners: the timer holds the deadline, `settled`
        // and `refused` are each moved into their own promise chain. None is
        // dropped, because the browser keeps only a borrowed reference to each;
        // `forget()` leaks them deliberately, and leaking them is what a
        // callback that must outlive the frame costs. They are separate
        // `Closure::once` values, so none can fire twice into a moved
        // `resolve`/`reject` — the same handler wired twice would move `reject`
        // into the first closure and fail to compile, not double-report.
        let _ = promise.then(&settled).catch(&refused);
        settled.forget();
        refused.forget();
    })
    .await;
    match outcome {
        Ok(value) => Ok(value),
        Err(tagged) => match decode(tagged) {
            Some(error) => Err(RacedFailure::Refused(error)),
            None => Err(RacedFailure::Stalled(step)),
        },
    }
}

/// Tag a failure so the two shapes survive the trip through one reject channel.
///
/// The stall carries no payload: the deadline belongs to the step that started
/// it, so the reader recovers which one it was from `raced`'s own argument.
fn tag(kind: f64, value: &JsValue) -> JsValue {
    js_sys::Array::of2(&JsValue::from_f64(kind), value).into()
}

/// The rejection a tagged failure carries, or `None` when it is the stall.
///
/// Anything that is not a readable refusal tag — a value this crate never
/// tagged, a tag whose kind is not a number, a bare number — reads as the
/// stall on purpose. That is the safe direction: an unreadable tag must not be
/// dressed up as a refusal the browser never gave, and the stall caption is
/// the honest "we stopped waiting" rather than a fabricated reason.
fn decode(value: JsValue) -> Option<JsValue> {
    let array = value.dyn_into::<js_sys::Array>().ok()?;
    match array.get(0).as_f64() {
        Some(0.0) => Some(array.get(1)),
        Some(_) | None => None,
    }
}
