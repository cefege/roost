//! What a recording leaves in the draft, and what it must never leave there.
//!
//! THE REGRESSION. Dictation does not own a field of its own: it paints onto a
//! draft an operator is also typing in, and every word it produces goes through
//! three different endings — a finalize answer, the deadline that waits for one,
//! and a composer that simply went away mid-recording. Each has to move the
//! draft exactly once, and each arrives on its own schedule: a stopped stream
//! answers its finalize on the wire AND leaves a timer running in case that
//! answer never comes. A page that answers both commits the same sentence
//! twice; a page that lets a timer from a finished recording answer for the next
//! one ends a recording the operator is still speaking into.
//!
//! The engine that produces these events is browser-only (`voice::deepgram_engine`
//! is compiled for wasm32), so what is pinned here is the half a browser cannot
//! change: the state machine's decisions in the order the engine makes them, the
//! fence that decides which recording an answer belongs to, and the draft they
//! produce. The composer's own binding is a bundle of signals that needs a live
//! component; the pure painter it is built from is not, and it is the same
//! `voice::transcript` the component calls.
//!
//! Test root, so the unwrap allowance is declared here (`CLAUDE.md` "the test
//! exemption reaches a test binary and not a fixture").

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_web::voice::state::{
    LiveTranscript, RunFence, VoiceEffect, VoiceEvent, VoiceMachine, VoiceState,
};
use roost_web::voice::transcript::{PaintedDraft, glued, paint};

/// One composer and the mic in it, driven through the doors the component uses.
#[derive(Default)]
struct Composer {
    machine: VoiceMachine,
    /// The draft the current recording started from, latched on its first word.
    base: Option<String>,
    /// The field's value, which is what the smoke specs read off `chat-input`.
    field: String,
    provisional_from: Option<usize>,
}

impl Composer {
    /// The tap, which opens a recording from idle and stops one that is open.
    fn tap(&mut self) {
        let effects = self.machine.apply(VoiceEvent::Toggle {
            active: true,
            claimed: true,
            engine_available: true,
        });
        self.perform(effects);
    }

    /// The engine's answer that the device attached AND the socket opened.
    fn live(&mut self) {
        let effects = self.machine.apply(VoiceEvent::Live);
        self.perform(effects);
    }

    /// One engine transcript, painted while the mic is recording.
    fn transcript(&mut self, settled: &str, hypothesis: &str) {
        self.machine.apply_transcript(settled, hypothesis);
        if !self.machine.state().is_dictating() {
            return;
        }
        if self.base.is_none() {
            self.base = Some(self.field.clone());
        }
        let painted = paint(
            self.base.as_deref().unwrap_or_default(),
            &LiveTranscript {
                settled: settled.to_owned(),
                hypothesis: hypothesis.to_owned(),
            },
        );
        self.field = painted.text;
        self.provisional_from = painted.provisional_from;
    }

    /// The engine's ending.
    fn settle(&mut self) {
        let effects = self.machine.apply(VoiceEvent::Settled);
        self.perform(effects);
    }

    /// The composer going away mid-recording, which is what the mobile drawer
    /// does to the only composer on screen.
    fn unmount(&mut self) {
        let effects = self.machine.apply(VoiceEvent::Deactivated);
        self.perform(effects);
    }

    /// Perform what the machine asked for, the way the composer does.
    fn perform(&mut self, effects: Vec<VoiceEffect>) {
        for effect in effects {
            match effect {
                VoiceEffect::Commit(words) => {
                    let base = self.base.take().unwrap_or_else(|| self.field.clone());
                    self.provisional_from = None;
                    self.field = glued(&base, &words);
                }
                VoiceEffect::DiscardRecording => {
                    self.provisional_from = None;
                    if let Some(base) = self.base.take() {
                        self.field = base;
                    }
                }
                _ => {}
            }
        }
    }

    /// One whole recording: tapped open, live, tapped shut, answered, settled.
    fn record(&mut self, words: &str) {
        self.tap();
        self.live();
        self.tap();
        self.transcript(words, "");
        self.settle();
    }

    /// The words of the hypothesis currently painted over the draft.
    fn hypothesis(&self) -> String {
        self.machine.live_update().hypothesis
    }

    /// The draft as a send would carry it: what is in the field, unproven tail
    /// included, because the field is what the operator is looking at.
    fn field(&self) -> &str {
        &self.field
    }

    /// The draft as the store holds it.
    fn stored(&self) -> String {
        PaintedDraft {
            text: self.field.clone(),
            provisional_from: self.provisional_from,
        }
        .persisted()
    }
}

#[test]
fn a_recording_that_hears_words_commits_them_once_and_leaves_no_hypothesis() {
    let mut composer = Composer::default();
    composer.tap();
    composer.live();
    composer.transcript("", "still recording");
    assert_eq!(
        composer.hypothesis(),
        "still recording",
        "the interim a recording is painting is the machine's own, or the fixture proves nothing"
    );

    composer.tap();
    composer.transcript("hello from the mic", "");
    composer.settle();

    assert_eq!(composer.field(), "hello from the mic");
    assert_eq!(
        composer.hypothesis(),
        "",
        "a recording that has ended leaves the hypothesis it was holding in the draft"
    );
    assert_eq!(composer.machine.state(), VoiceState::Idle);
    assert_eq!(
        composer.stored(),
        "hello from the mic",
        "a settled recording is ordinary text the next composer may keep"
    );
}

#[test]
fn the_deadline_that_waits_for_a_finalize_cannot_settle_it_a_second_time() {
    let mut composer = Composer::default();
    composer.record("hello from the mic");

    // The answer has already settled this recording; the deadline left running
    // in case it never arrived now fires anyway.
    composer.settle();

    assert_eq!(
        composer.field(),
        "hello from the mic",
        "the same sentence must not be committed twice into a draft the operator is about to \
         send"
    );
    assert_eq!(composer.machine.state(), VoiceState::Idle);
}

#[test]
fn a_third_recording_settles_onto_exactly_what_the_two_before_it_committed() {
    let mut composer = Composer::default();
    for _ in 0..3 {
        composer.record("hello from the mic");
    }

    assert_eq!(
        composer.field(),
        "hello from the mic hello from the mic hello from the mic",
        "three consecutive recordings each append their own words, and none of them commits a \
         recording's words twice"
    );
    assert_eq!(composer.machine.state(), VoiceState::Idle);
}

#[test]
fn a_token_from_the_recording_the_fence_moved_past_is_not_admitted() {
    let mut fence = RunFence::default();
    let first = fence.begin();
    let second = fence.begin();

    assert_eq!(fence.current(), second);
    assert!(
        fence.admits(second),
        "the recording that is open answers to the token it was opened with"
    );
    assert!(
        !fence.admits(first),
        "the socket, the finalize deadline and the key handoff of a recording that has ended must \
         not be admitted into the draft of the recording that replaced it"
    );
    assert!(
        fence.claim_settle(),
        "a recording that is open settles when its answer arrives"
    );
    assert!(
        !fence.claim_settle(),
        "the deadline that waited for that answer settles nothing a second time"
    );
    assert!(
        !fence.admits(first),
        "a finished recording's token stays refused once the next one has opened"
    );

    fence.begin();
    assert!(
        fence.claim_settle(),
        "settling one recording must not spend the next one's settle"
    );
}

#[test]
fn words_from_a_recording_that_has_already_ended_do_not_reach_the_draft() {
    let mut composer = Composer::default();
    composer.record("hello from the mic");

    // The browser delivers to a socket it never closed.
    composer.transcript("", "stale words");
    assert_eq!(
        composer.field(),
        "hello from the mic",
        "a word from a recording that has ended is not painted over the draft the operator is \
         still reading"
    );

    composer.tap();
    composer.live();
    composer.transcript("", "still recording");
    assert_eq!(
        composer.field(),
        "hello from the mic still recording",
        "the recording that replaced it is still hearing, and still painting"
    );
}

#[test]
fn a_recording_the_operator_walks_away_from_leaves_the_draft_it_started_from() {
    let mut composer = Composer::default();
    composer.record("hello from the mic");

    composer.tap();
    composer.live();
    composer.transcript("", "still recording");
    assert_eq!(composer.stored(), "hello from the mic");

    // The drawer covers the only composer on screen, which unmounts it.
    composer.unmount();

    assert_eq!(
        composer.field(),
        "hello from the mic",
        "a composer that went away mid-hypothesis must leave the draft it started from, and not \
         a guess the recognizer never committed to"
    );
    assert_eq!(composer.machine.state(), VoiceState::Idle);
    assert_eq!(
        composer.stored(),
        "hello from the mic",
        "what the next composer reads back is the draft without the guess"
    );
}
