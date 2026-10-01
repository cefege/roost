//! How one recording ENDS: an operator's stop, a finalize that arrives or does
//! not, a socket that closed, and a failure that must be reported once.
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

impl Deepgram {
    /// Stop with the intent to insert what was heard.
    pub fn stop(&self) {
        self.end(CloseIntent::Send);
    }

    /// Stop without inserting anything.
    pub fn abort(&self) {
        self.end(CloseIntent::Cancel);
    }

    fn end(&self, intent: CloseIntent) {
        let socket = {
            let mut session = self.session();
            session.end_intent = Some(intent);
            session.socket.clone()
        };
        let Some(socket) = socket else {
            return;
        };
        if socket.ready_state() != WebSocket::OPEN {
            return;
        }
        let _ = socket.send_with_str("{\"type\":\"Finalize\"}");
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
    /// stopped stream answers its finalize on the wire AND leaves a deadline
    /// running in case that answer never comes, and the second of the two to
    /// arrive would report the same words a second time — which the composer
    /// appends to the draft a second time. A cancelled or failed recording is
    /// not settled at all: its words are not the draft's.
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
        // What a finished recording leaves behind: a keepalive that would keep
        // a dead stream open, and a socket nothing will read again.
        self.stop_keepalive();
        if let Some(socket) = socket {
            let _ = socket.send_with_str("{\"type\":\"CloseStream\"}");
            let _ = socket.close();
        }
        (self.sink)(EngineEvent::Transcript {
            settled,
            hypothesis: String::new(),
        });
        (self.sink)(EngineEvent::Settled);
        super::audio_capture::stop_capture();
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
