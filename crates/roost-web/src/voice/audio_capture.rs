//! The warm microphone: one audio graph per page, opened on the tap and kept for
//! the next recording.
//!
//! The graph is a singleton for one reason — a browser charges a device open,
//! and iOS only starts an audio session inside a fresh tap — so the open happens
//! on pointer-down and the recording that follows joins the open already in
//! flight rather than starting a second. This file owns that singleton and the
//! lifecycle; `super::capture_open` owns the ordered walk that builds it, and
//! `super::audio_graph` the two ways frames are read out of it.
//! Ports `apps/web/src/voice/audioPcmCapture.ts`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use js_sys::Promise;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{AudioContext, Event, MediaStream, MessageEvent};

use super::capture_facts::CapturePath;
pub use super::capture_facts::{CaptureFacts, capture_stats, idle_release_ms, is_warm};
pub use super::capture_open::state_name;
use super::capture_open::{open_pipeline, stop_stream};
pub use super::open_device::OpenListener;
use super::open_device::OpenSlot;
pub use super::pcm::DEFAULT_INPUT_RATE;
use super::pcm::Resampler;

/// The worklet's own source, inline.
///
/// A separate module file would have to be fetched before the mic could open,
/// and a failed fetch would be a failed tap; a blob URL built from this constant
/// is available the instant the tap is. Ports `WORKLET_SRC` verbatim.
pub const WORKLET_SOURCE: &str = r#"
class PcmForwarder extends AudioWorkletProcessor {
  process(inputs) {
    const ch = inputs[0] && inputs[0][0];
    if (ch && ch.length) this.port.postMessage(ch.slice(0));
    return true;
  }
}
registerProcessor('pcm-forwarder', PcmForwarder);
"#;

/// The name the worklet registers its processor under.
pub const WORKLET_PROCESSOR: &str = "pcm-forwarder";

/// How long an open device may take before the tap counts as stalled.
pub const OPEN_TIMEOUT_MS: i32 = 6_000;

/// How long the worklet module may take to load.
pub const MODULE_TIMEOUT_MS: i32 = 3_000;

/// How long the graph is kept after a recording ends. A minute on a desktop,
/// where the next recording may be minutes away.
pub const IDLE_RELEASE_MS: i32 = 60_000;

/// The idle release a touch device uses: the next tap comes from the same hand,
/// seconds later.
pub const TOUCH_IDLE_RELEASE_MS: i32 = 4_000;

/// A chunk consumer: linear16 bytes, ready to put on the wire.
pub type ChunkSink = Rc<dyn Fn(Vec<u8>)>;
/// The audio callbacks this graph owns. Held so a release drops them with the
/// graph, rather than leaving a port that outlives the node it reports from.
/// `super::audio_graph` builds them; this owns them.
pub(super) type WorkletHandler = wasm_bindgen::closure::Closure<dyn FnMut(MessageEvent)>;
pub(super) type ProcessorHandler = wasm_bindgen::closure::Closure<dyn FnMut(Event)>;

/// One open microphone, owned by the page.
#[derive(Default)]
pub(super) struct MicCapture {
    pub(super) context: Option<AudioContext>,
    pub(super) stream: Option<MediaStream>,
    pub(super) sink: Option<ChunkSink>,
    pub(super) resampler: Option<Resampler>,
    pub(super) worklet_handler: Option<WorkletHandler>,
    pub(super) processor_handler: Option<ProcessorHandler>,
    pub(super) path: Cell<CapturePath>,
    pub(super) frames: Cell<usize>,
    pub(super) peak: Cell<f32>,
    /// Latched after one worklet failure: a module that will not load on this
    /// browser will not load on the next tap either.
    pub(super) worklet_broken: Cell<bool>,
    /// Incremented by every release, so a frame that arrives after a teardown
    /// cannot resurrect a graph that is gone, and by every fresh open, so two
    /// graphs in one page cannot both claim the frames.
    pub(super) generation: Cell<u64>,
    /// The open in flight, and the decision of who opens it.
    pub(super) open: OpenSlot,
}

thread_local! {
    static MIC: RefCell<MicCapture> = RefCell::new(MicCapture::default());
}

/// Run a closure against the page's microphone.
pub(super) fn with_mic<R>(visit: impl FnOnce(&mut MicCapture) -> R) -> R {
    MIC.with(|mic| visit(&mut mic.borrow_mut()))
}

/// Open the device and start delivering chunks to `sink`.
///
/// Every caller while an open is in flight joins that one open rather than
/// starting a second: the tap that warms the device and the recording that
/// follows are the same device charge, and racing a second open against a
/// parked one is exactly how a page ends up awaiting a promise that never
/// settles. `listener` is told what the open decided, either way.
pub fn start_capture(sink: ChunkSink, listener: OpenListener) {
    let (open, started) = with_mic(|mic| {
        mic.sink = Some(sink);
        mic.resampler = Some(Resampler::new(DEFAULT_INPUT_RATE));
        mic.frames.set(0);
        mic.peak.set(0.0);
        mic.open.join_or_start()
    });
    open.join(listener);
    if !started {
        return;
    }
    let generation = with_mic(|mic| {
        mic.generation.set(mic.generation.get() + 1);
        mic.generation.get()
    });
    wasm_bindgen_futures::spawn_local(async move {
        let failure = open_pipeline(generation).await.err();
        if let Some(failure) = &failure {
            tracing::warn!(
                target: "voice",
                name = %failure.name,
                detail = %failure.detail,
                "microphone open refused"
            );
        }
        // Released before the verdict is delivered, so a listener that taps
        // again from inside its own callback opens a fresh device instead of
        // joining this one.
        with_mic(|mic| mic.open.release());
        // The classified caption, not the browser's own message: this string is
        // what an operator reads under the mic. `detail` went to the
        // diagnostics event above, which is where a raw refusal belongs.
        open.settle(failure.map(|failure| failure.message));
    });
}

/// Open the graph if it is not already open, warming it for the tap that
/// follows.
pub fn warm_mic() {
    if is_warm() {
        return;
    }
    let sink: ChunkSink = Rc::new(|_chunk| {});
    start_capture(sink, Rc::new(|_failure| {}));
}

/// Whether a device open is still running and has not answered.
///
/// The deadlines that watch a recording all ask whether the device arrived,
/// and none of them may answer it while the open is still in flight: the open
/// knows WHICH step stalled, and a timer that decides first replaces that
/// caption with one that only says to tap again.
#[must_use]
pub fn open_pending() -> bool {
    with_mic(|mic| mic.open.pending())
}

/// Load the inline worklet and connect the graph through it.
/// Turn float frames into linear16 chunks and hand them to the sink.
pub(super) fn deliver(frames: &[f32], generation: u64) {
    with_mic(|mic| {
        if mic.generation.get() != generation {
            return;
        }
        let peak = frames
            .iter()
            .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
        mic.peak.set(mic.peak.get().max(peak));
        let Some(sink) = mic.sink.clone() else {
            return;
        };
        let chunks = mic
            .resampler
            .as_mut()
            .map_or_else(Vec::new, |resampler| resampler.push(frames));
        for chunk in chunks {
            mic.frames.set(mic.frames.get() + 1);
            sink(chunk);
        }
    });
}

/// Stop delivering frames, keeping the graph open for the next recording.
///
/// The graph's own identity is untouched: a recording that ends is a sink going
/// away, and a warm graph the next tap reuses is the SAME graph, still
/// entitled to deliver to whoever holds the sink then.
pub fn stop_capture() {
    with_mic(|mic| mic.sink = None);
}

/// Tear the graph down: stop every track and close the context.
///
/// A closed context cannot be reopened on iOS without a fresh tap, which is why
/// this is only reached by the idle release and by a repair.
pub fn release_mic() {
    with_mic(|mic| {
        mic.generation.set(mic.generation.get() + 1);
        // The open in flight is worthless now, and leaving it in the slot is
        // what let one stalled open poison every later tap in the page.
        mic.open.release();
        if let Some(stream) = mic.stream.take() {
            stop_stream(&stream);
        }
        if let Some(context) = mic.context.take() {
            let _ = context.close();
        }
        mic.sink = None;
        mic.resampler = None;
        mic.worklet_handler = None;
        mic.processor_handler = None;
        mic.path.set(CapturePath::None);
        mic.frames.set(0);
        mic.peak.set(0.0);
    });
}

/// Rebuild the pipeline once, on a graph that opened but delivered silence.
pub fn repair_capture(sink: ChunkSink, listener: OpenListener) {
    with_mic(|mic| mic.worklet_broken.set(true));
    release_mic();
    start_capture(sink, listener);
}

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
