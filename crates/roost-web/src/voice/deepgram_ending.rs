//! How one recording ENDS: an operator's stop, a close the service answers or
//! does not, a socket that closed, and a failure that must be reported once.
//!
//! Split out of `super::deepgram_engine` because ending is the one part of the
//! protocol with a decision to get wrong rather than a call to make: every
//! branch here releases the device, the socket and the timers, and a branch that
//! runs twice reports a second reason for the same failure over the first.
//! Called by `super::engine_events`' socket callbacks and by the component that
//! owns the engine.

use std::rc::Weak;

use web_sys::WebSocket;

use super::deepgram_engine::{Deepgram, FINALIZE_WAIT_MS, RECONNECT_DELAY_MS};
use super::engine_events::{EngineEvent, after};
use super::handshake::{CloseIntent, close_message, is_expected_close};

/// Ask the service to finish the stream: it answers the audio it still holds,
/// sends a `Metadata` summary, and closes.
const CLOSE_STREAM: &str = "{\"type\":\"CloseStream\"}";

impl Deepgram {
    /// Stop with the intent to insert what was heard.
    pub fn stop(&self) {
        self.end(CloseIntent::Send);
    }

    /// Stop without inserting anything.
    pub fn abort(&self) {
        self.end(CloseIntent::Cancel);
    }

    /// End the recording: the device stops now, and the stream is asked to
    /// finish (`Send`) or is hung up on (`Cancel`).
    ///
    /// The device stops at the press because a word spoken after Stop is not
    /// this recording's, and a discarded one must not keep streaming. A stop
    /// is `CloseStream` rather than `Finalize`: Deepgram answers a `Finalize`
    /// only while it still holds unanswered audio, and without words when its
    /// own endpointing has already finalized the last phrase — exactly the stop
    /// after a sentence the operator can already read — so nothing would settle
    /// before the deadline. A `CloseStream` is answered every time: the last
    /// results, the summary `fold` settles on, then the close `closed` settles on.
    fn end(&self, intent: CloseIntent) {
        let socket = {
            let mut session = self.session();
            // One ending per recording, except that a stop still waiting for
            // its words may be abandoned.
            let ended = match session.end_intent {
                None => false,
                Some(CloseIntent::Send) => intent != CloseIntent::Cancel,
                Some(_) => true,
            };
            if ended {
                return;
            }
            session.end_intent = Some(intent);
            session.socket.clone()
        };
        super::audio_capture::stop_capture();
        self.stop_keepalive();
        let open = socket
            .as_ref()
            .is_some_and(|socket| socket.ready_state() == WebSocket::OPEN);
        tracing::debug!(target: "voice", ?intent, socket_open = open, "voice.recording_ended");
        if intent != CloseIntent::Send {
            self.session().socket = None;
            if let Some(socket) = socket {
                if open {
                    let _ = socket.send_with_str(CLOSE_STREAM);
                }
                let _ = socket.close();
            }
            return;
        }
        let Some(socket) = socket.filter(|_| open) else {
            // Between a dropped socket and its reconnect nothing can answer, so
            // what has settled is everything this recording will have.
            self.settle();
            return;
        };
        let _ = socket.send_with_str(CLOSE_STREAM);
        let run = self.current_run();
        let weak = self.weak();
        after(FINALIZE_WAIT_MS, move || {
            if let Some(engine) = weak.as_ref().and_then(Weak::upgrade)
                && engine.admits(run)
            {
                engine.settle();
            }
        });
    }

    /// The recording is over: the words are settled, whatever the socket did.
    ///
    /// Once, and only for an operator who asked to keep what was heard. A
    /// stopped stream answers its close with a summary AND a close, and leaves
    /// a deadline running in case neither comes; whichever arrives second
    /// would report the same words a second time — which the composer appends
    /// to the draft a second time. A cancelled or failed recording is not
    /// settled at all: its words are not the draft's.
    pub(super) fn settle(&self) {
        let socket = {
            let mut session = self.session();
            if session.end_intent != Some(CloseIntent::Send) {
                return;
            }
            if !self.claim_settle() {
                return;
            }
            session.socket.take()
        };
        let (settled, heard_nothing) = {
            let session = self.session.borrow();
            (session.settled(), session.results == 0)
        };
        if heard_nothing && settled.is_empty() {
            tracing::warn!(target = "voice", "voice.dictation_empty");
        }
        // `end` already stopped the device and the keepalive and asked the
        // service to close; what is left is our end of a socket nothing reads.
        if let Some(socket) = socket {
            let _ = socket.close();
        }
        (self.sink)(EngineEvent::Transcript {
            settled,
            hypothesis: String::new(),
        });
        (self.sink)(EngineEvent::Settled);
    }

    /// Report a failure, ending the recording ONCE.
    ///
    /// Three deadlines watch a recording, and the one that loses the race must
    /// not win the caption: a `getUserMedia` refused in a millisecond would
    /// otherwise be overwritten nine seconds later by the start grace, which
    /// knows only that the device never attached. The first reason is the one
    /// the operator can act on, so the later ones are dropped rather than
    /// reported, and the device and socket this failure abandons are released
    /// here rather than by whoever armed the timer.
    pub(super) fn fail(&self, caption: &str) {
        let socket = {
            let mut session = self.session();
            if session.failed {
                return;
            }
            session.failed = true;
            // An operator who asked to stop still wants the words, so only an
            // intent nobody set becomes a cancellation — which is also what
            // tells `closed` that this close was asked for.
            if session.end_intent.is_none() {
                session.end_intent = Some(CloseIntent::Cancel);
            }
            session.socket.clone()
        };
        super::audio_capture::stop_capture();
        self.stop_keepalive();
        if let Some(socket) = socket
            && socket.ready_state() == WebSocket::OPEN
        {
            let _ = socket.close();
        }
        (self.sink)(EngineEvent::Failed(caption.to_owned()));
    }

    /// The socket closed for a reason nobody asked for.
    pub(super) fn closed(&self, code: u16, reason: &str) {
        self.session().socket = None;
        self.stop_keepalive();
        let intent = self.session.borrow().end_intent;
        if is_expected_close(code, intent) {
            if intent == Some(CloseIntent::Send) {
                self.settle();
            }
            return;
        }
        let should_retry = {
            let mut session = self.session();
            let retry = !session.retried;
            session.retried = true;
            retry
        };
        if should_retry {
            tracing::warn!(target: "voice", stage = "ws", code, "voice.ws_retry");
            let run = self.current_run();
            let weak = self.weak();
            after(RECONNECT_DELAY_MS, move || {
                if let Some(engine) = weak.as_ref().and_then(Weak::upgrade)
                    && engine.admits(run)
                {
                    engine.connect(run);
                }
            });
            return;
        }
        tracing::warn!(target: "voice", stage = "close", code, reason, "voice.ws_failed");
        self.fail(&close_message(code, reason));
        if intent == Some(CloseIntent::Send) {
            self.settle();
        }
    }
}
