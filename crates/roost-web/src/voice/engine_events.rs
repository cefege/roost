//! What an engine tells its owner, and the timers and socket callbacks that
//! carry those reports back.
//!
//! Split out of `super::deepgram_engine` because the vocabulary is shared with
//! the browser's own recognizer — both answer the same four events — and because
//! a closure that must outlive the call that made it deserves its own file
//! rather than living at the bottom of the engine that arms it.

use std::rc::{Rc, Weak};

use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{CloseEvent, ErrorEvent, MessageEvent, WebSocket};

use super::deepgram_engine::{Deepgram, KEEPALIVE_MS, SILENCE_GRACE_MS, START_GRACE_MS};
use super::handshake::captions;

/// Attach the four socket callbacks, fenced to the recording that opened them.
///
/// `onerror` is deliberately empty: `onclose` owns every decision, and an error
/// frame would only produce a second, worse caption for the same failure.
///
/// The fence is the run, because a socket outlives the tap that opened it: a
/// finalize answer that lands after the next recording has started is a word
/// from the PREVIOUS conversation, and without this it is appended to a draft
/// the operator is still typing into.
pub(super) fn bind_socket(socket: &WebSocket, engine: &Rc<Deepgram>, run: u64) {
    let weak = engine.owner.borrow().clone();
    let on_open = Closure::wrap(Box::new(move || {
        if let Some(engine) = live(&weak, run) {
            engine.opened();
        }
    }) as Box<dyn FnMut()>);
    socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));
    on_open.forget();

    let weak = engine.owner.borrow().clone();
    let on_message = Closure::wrap(Box::new(move |event: MessageEvent| {
        if let Some(engine) = live(&weak, run) {
            engine.inbound_frame(run, event.data());
        }
    }) as Box<dyn FnMut(MessageEvent)>);
    socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    on_message.forget();

    let weak = engine.owner.borrow().clone();
    let on_close = Closure::wrap(Box::new(move |event: CloseEvent| {
        if let Some(engine) = live(&weak, run) {
            engine.closed(event.code(), &event.reason());
        }
    }) as Box<dyn FnMut(CloseEvent)>);
    socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));
    on_close.forget();

    let on_error = Closure::wrap(Box::new(|_event: ErrorEvent| {}) as Box<dyn FnMut(ErrorEvent)>);
    socket.set_onerror(Some(on_error.as_ref().unchecked_ref()));
    on_error.forget();
}

/// The engine, if it is still on the recording that opened this socket.
fn live(weak: &Option<Weak<Deepgram>>, run: u64) -> Option<Rc<Deepgram>> {
    let engine = weak.as_ref().and_then(Weak::upgrade)?;
    engine.admits(run).then_some(engine)
}

/// Run `visit` once, on a timer.
pub(super) fn after<F: FnMut() + 'static>(millis: i32, mut visit: F) {
    if let Some(window) = web_sys::window() {
        let closure = Closure::once(move || visit());
        let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
            closure.as_ref().unchecked_ref(),
            millis,
        );
        closure.forget();
    }
}

/// Run `visit` on a repeating timer, returning the handle that stops it.
pub(super) fn interval(visit: Closure<dyn FnMut()>, millis: i32) -> Option<i32> {
    let handle = web_sys::window().and_then(|window| {
        window
            .set_interval_with_callback_and_timeout_and_arguments_0(
                visit.as_ref().unchecked_ref(),
                millis,
            )
            .ok()
    });
    // The timer owns the callback for as long as the handle lives, and the
    // handle is cleared when the socket closes.
    visit.forget();
    handle
}

/// Everything an engine tells its owner. Both engines answer the same four
/// events, which is what lets the component own one state machine instead of two.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineEvent {
    /// The device attached AND the transport opened: the recording is genuinely
    /// live, which is the only thing that promotes the UI out of `starting`.
    Live,
    /// Words so far: finalized ones, and the hypothesis that may be replaced.
    Transcript {
        /// Finalized words, space-joined.
        settled: String,
        /// The current hypothesis.
        hypothesis: String,
    },
    /// The engine is done, one way or another.
    Settled,
    /// The engine gave up, with the caption to show.
    Failed(String),
}

/// Where an owner hears about the engine.
pub type EngineSink = Rc<dyn Fn(EngineEvent)>;

/// The engine's own timers, as an extension of the engine they watch.
///
/// Split out because these are the three deadlines a recording runs on, and each
/// one is a decision about when to GIVE UP rather than about the protocol.
impl Deepgram {
    /// Give the device `START_GRACE_MS` to attach, then refuse the tap — but
    /// never while the open itself is still running, because that open names
    /// the step which stalled and this deadline can only say "try again".
    pub(super) fn arm_start_grace(self: &Rc<Self>, run: u64) {
        let weak = self.weak();
        after(START_GRACE_MS, move || {
            let Some(engine) = weak.as_ref().and_then(Weak::upgrade) else {
                return;
            };
            // The recording this deadline watches is over: another one is open,
            // and a timer that outlived it may not decide for this one.
            if !engine.admits(run) {
                return;
            }
            if engine.session.borrow().failed {
                return;
            }
            if super::audio_capture::open_pending() {
                return;
            }
            if !engine.session.borrow().mic_attached {
                tracing::warn!(target: "voice", stage = "start_stalled", "voice.mic_failed");
                super::audio_capture::release_mic();
                engine.fail(captions::START_STALLED);
            }
        });
    }

    /// A graph that opened and delivered nothing is rebuilt once, then refused.
    /// A device that never attached is not that case, and judging it here
    /// reported the silence of an open that had not answered yet.
    pub(super) fn arm_silence_watch(self: &Rc<Self>, run: u64) {
        let weak = self.weak();
        after(SILENCE_GRACE_MS, move || {
            let Some(engine) = weak.as_ref().and_then(Weak::upgrade) else {
                return;
            };
            if !engine.admits(run) {
                return;
            }
            if engine.session.borrow().failed {
                return;
            }
            if super::audio_capture::open_pending() {
                return;
            }
            if super::audio_capture::capture_stats().frames > 0 {
                return;
            }
            let repair = {
                let mut session = engine.session();
                let repair = !session.repaired;
                session.repaired = true;
                repair
            };
            if repair {
                tracing::warn!(target: "voice", stage = "silent_retry", "voice.mic_failed");
                super::audio_capture::repair_capture(engine.chunk_sink(), engine.open_listener());
                engine.arm_silence_watch(run);
            } else {
                tracing::warn!(target: "voice", stage = "silent", "voice.mic_failed");
                engine.fail(captions::SILENT);
            }
        });
    }

    /// Keep the stream alive, because an idle socket is closed by the service.
    pub(super) fn arm_keepalive(&self) {
        if self.keepalive.borrow().is_some() {
            return;
        }
        let weak = self.weak();
        let tick = Closure::wrap(Box::new(move || {
            let Some(engine) = weak.as_ref().and_then(Weak::upgrade) else {
                return;
            };
            let socket = engine.session.borrow().socket.clone();
            if let Some(socket) = socket {
                if socket.ready_state() == WebSocket::OPEN {
                    let _ = socket.send_with_str("{\"type\":\"KeepAlive\"}");
                }
            }
        }) as Box<dyn FnMut()>);
        *self.keepalive.borrow_mut() = interval(tick, KEEPALIVE_MS);
    }

    /// Stop the keepalive, so a closed socket is not kept alive by the page.
    pub(super) fn stop_keepalive(&self) {
        if let Some(id) = self.keepalive.borrow_mut().take()
            && let Some(window) = web_sys::window()
        {
            window.clear_interval_with_handle(id);
        }
    }
}
