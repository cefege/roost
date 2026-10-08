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

use web_sys::{AudioContext, AudioContextState, Event, MediaStream, MessageEvent};

use super::capture_facts::CapturePath;
pub use super::capture_facts::{CaptureFacts, capture_stats, idle_release_ms, is_warm};
pub use super::capture_open::state_name;
use super::capture_open::{open_pipeline, stop_stream};
use super::idle_release::{
    ReleaseHandler, arm_idle_release, clear_idle_release, watch_page_visibility,
};
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

/// How long the graph is kept after a recording ends on a desktop: not at all.
/// A desktop reopens its device in milliseconds without a fresh gesture, and
/// a graph kept warm keeps the browser's recording indicator lit while nothing
/// is recording.
pub const IDLE_RELEASE_MS: i32 = 0;

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
    /// The pending idle release, if one is armed.
    pub(super) idle_timer: Option<i32>,
    /// The idle timer's callback, built once and kept for the page's life so a
    /// timer that fires never calls into a freed closure.
    pub(super) idle_handler: Option<ReleaseHandler>,
    /// The page-hidden listener, installed by the first open.
    pub(super) visibility_handler: Option<ReleaseHandler>,
}

thread_local! {
    static MIC: RefCell<MicCapture> = RefCell::new(MicCapture::default());
}

/// Run a closure against the page's microphone.
pub(super) fn with_mic<R>(visit: impl FnOnce(&mut MicCapture) -> R) -> R {
    MIC.with(|mic| visit(&mut mic.borrow_mut()))
}

/// Attach `sink` and start delivering chunks to it.
///
/// A graph that is already open and running is reused: a second `getUserMedia`
/// would publish a new stream over the old one, and the old one's tracks — no
/// longer referenced by anything that stops them — keep the browser's
/// recording indicator lit for the rest of the page's life. Otherwise every
/// caller while an open is in flight joins that one open rather than starting
/// a second: the tap that warms the device and the recording that follows are
/// the same device charge, and racing a second open against a parked one is
/// exactly how a page ends up awaiting a promise that never settles.
/// `listener` is told what the open decided, either way.
pub fn start_capture(sink: ChunkSink, listener: OpenListener) {
    if attach_to_warm_graph(&sink) {
        listener(None);
        return;
    }
    open_shared(Some(sink), listener);
}

/// Open the graph if it is not already open, warming it for the tap that
/// follows. A graph that is already warm restarts its idle release instead.
pub fn warm_mic() {
    if is_warm() {
        arm_idle_release();
        return;
    }
    open_shared(None, Rc::new(|_failure| {}));
}

/// Hand `sink` the frames of the graph already open, when it still runs.
///
/// A suspended context renders nothing — iOS suspends it on a lock, a call or
/// backgrounding and does not always resume it — so a warm graph that is not
/// running is released and the caller opens a fresh one.
fn attach_to_warm_graph(sink: &ChunkSink) -> bool {
    let running_rate = with_mic(|mic| {
        let context = mic.context.as_ref()?;
        let running =
            mic.path.get() != CapturePath::None && context.state() == AudioContextState::Running;
        running.then(|| context.sample_rate() as u32)
    });
    let Some(rate) = running_rate else {
        if is_warm() {
            tracing::info!(target: "voice", "voice.mic_reopen_suspended");
            release_mic();
        }
        return false;
    };
    clear_idle_release();
    with_mic(|mic| {
        mic.sink = Some(Rc::clone(sink));
        mic.resampler = Some(Resampler::new(rate));
        mic.frames.set(0);
        mic.peak.set(0.0);
    });
    tracing::debug!(target: "voice", rate, "voice.mic_reused");
    true
}

/// Join the open in flight or start one, attaching `sink` when there is one.
fn open_shared(sink: Option<ChunkSink>, listener: OpenListener) {
    let attaching = sink.is_some();
    let (open, started) = with_mic(|mic| {
        if let Some(sink) = sink {
            mic.sink = Some(sink);
            mic.resampler = Some(Resampler::new(DEFAULT_INPUT_RATE));
            mic.frames.set(0);
            mic.peak.set(0.0);
        }
        mic.open.join_or_start()
    });
    if attaching {
        clear_idle_release();
    }
    open.join(listener);
    if !started {
        return;
    }
    watch_page_visibility();
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
        // A warm-up no recording joined is idle from the moment it opens.
        arm_idle_release();
        // The classified caption, not the browser's own message: this string is
        // what an operator reads under the mic. `detail` went to the
        // diagnostics event above, which is where a raw refusal belongs.
        open.settle(failure.map(|failure| failure.message));
    });
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
    arm_idle_release();
}

/// Tear the graph down: stop every track and close the context.
///
/// A closed context cannot be reopened on iOS without a fresh tap, which is why
/// this is only reached by the idle release, a hidden page, a suspended graph,
/// and a repair.
pub fn release_mic() {
    clear_idle_release();
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
