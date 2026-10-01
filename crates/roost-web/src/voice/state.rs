//! The mic state machine, as a decision with no DOM in it.
//!
//! Ports the transition table of `apps/web/src/components/MobileVoiceInput.tsx`.
//! Every rule the Playwright oracles read off `data-state` — the starting gate,
//! the ownership claim, commit-on-switch, the finalize watchdog — is decided
//! here and returned as a list of effects, so each one can be pinned by a test
//! that has no microphone. The component in `crate::components::mobile_voice_input`
//! performs the effects; it does not decide anything.
//!
//! The `finalizing` state exists because a stop is not a stop: the engine has
//! audio still in flight, and the words must land in the composer rather than in
//! whatever the user typed while waiting.

/// Where one recording is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VoiceState {
    /// No recording.
    #[default]
    Idle,
    /// The device and the transport are opening. A tap here is deliberately
    /// inert: the second tap of an accidental double-tap must not cancel a start
    /// that is about to succeed.
    Starting,
    /// Recording.
    Listening,
    /// Stopped, waiting for the engine's final result before inserting.
    Finalizing,
}

impl VoiceState {
    /// The `data-state` value, verbatim the string v2 emits.
    #[must_use]
    pub fn data_state(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Starting => "starting",
            Self::Listening => "listening",
            Self::Finalizing => "finalizing",
        }
    }

    /// Whether a recording is open, across every non-idle state. The composer's
    /// send button and Enter key are inert while this is true, and the pad
    /// router's dictation lamp reads it.
    #[must_use]
    pub fn is_dictating(&self) -> bool {
        !matches!(self, Self::Idle)
    }
}

/// Which recording this page is on, and whether that recording has ended.
///
/// Both answers outlive the call that asked for them. A socket, a finalize
/// deadline and a key handoff in flight all keep answering after the tap that
/// armed them, and the recording they were asked about is usually over by then:
/// a word from a conversation the operator has already read is not a word for
/// the draft they are reading now. The counter is counted rather than reset,
/// because the socket of one recording is still open when the next one starts,
/// and a wiped counter would let the first answer for the second.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunFence {
    current: u64,
    ended: bool,
}

impl RunFence {
    /// Begin the next recording, returning the token it answers to.
    pub fn begin(&mut self) -> u64 {
        self.current += 1;
        self.ended = false;
        self.current
    }

    /// The token a callback must carry to be allowed to speak for the engine.
    #[must_use]
    pub fn current(&self) -> u64 {
        self.current
    }

    /// Whether a callback issued for `token` still belongs to this recording.
    #[must_use]
    pub fn admits(&self, token: u64) -> bool {
        token == self.current
    }

    /// Claim the one settle a recording gets.
    ///
    /// A finalize answer and the deadline that waits for it both ask to settle,
    /// and a second settle reports the same words again — which the composer
    /// would append to the draft a second time.
    pub fn claim_settle(&mut self) -> bool {
        if self.ended {
            return false;
        }
        self.ended = true;
        true
    }
}

/// What the composer asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceEvent {
    /// The mic (or the shell's dictation command) was activated. `active` is
    /// whether this composer is the one that may record.
    Toggle {
        /// Whether this composer may record right now.
        active: bool,
        /// Whether the microphone claim succeeded.
        claimed: bool,
        /// Whether an engine exists at all.
        engine_available: bool,
    },
    /// The engine reported live audio: the mic attached and the socket opened.
    Live,
    /// The engine delivered its final transcript, empty or not.
    Settled,
    /// The engine gave up; its caption is already shown.
    Failed,
    /// This composer stopped being the active one (a pane switch, a collapse).
    Deactivated,
    /// The finalize watchdog expired: the engine never settled.
    WatchdogExpired,
    /// The discard control was pressed.
    Discard,
}

/// One instruction to the component, in the order v2 ran it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceEffect {
    /// Clear the error caption.
    ClearError,
    /// Show this caption. Every string here is one a spec asserts on.
    ShowError(&'static str),
    /// Reset the engine's transcripts without touching its socket state.
    ClearEngineText,
    /// Start the chosen engine.
    StartEngine,
    /// Stop the engine with the intent to insert what it heard.
    StopAndSend,
    /// Stop the engine without inserting anything.
    AbortEngine,
    /// Take the voice slot for this composer.
    ClaimVoice,
    /// Release the voice slot if this composer holds it.
    ReleaseVoice,
    /// Arm the finalize watchdog.
    ArmFinalizeWatchdog,
    /// Cancel the finalize watchdog.
    ClearFinalizeWatchdog,
    /// Insert this text into the composer and go idle.
    Commit(String),
    /// Return the composer to its pre-recording text and go idle.
    DiscardRecording,
}

impl VoiceEffect {
    /// Insert words, or restore the composer when the engine heard nothing:
    /// v2's `deliver`, which sends an empty transcript down the discard path so
    /// the field does not grow a trailing space for nothing.
    fn deliver(text: String) -> Self {
        if text.is_empty() {
            Self::DiscardRecording
        } else {
            Self::Commit(text)
        }
    }
}

/// The whole of one mic's behaviour, minus the browser.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VoiceMachine {
    state: VoiceState,
    settled: String,
    hypothesis: String,
    error: Option<&'static str>,
}

impl VoiceMachine {
    /// The current state.
    #[must_use]
    pub fn state(&self) -> VoiceState {
        self.state
    }

    /// The error caption, empty when there is none.
    #[must_use]
    pub fn error(&self) -> Option<&'static str> {
        self.error
    }

    /// Fold an engine transcript update in. Interim text is replaced wholesale
    /// rather than appended, because the engine re-reads the same audio.
    pub fn apply_transcript(&mut self, settled: &str, hypothesis: &str) {
        if !settled.is_empty() {
            self.settled = settled.to_owned();
        }
        self.hypothesis = hypothesis.to_owned();
    }

    /// The words to insert when this recording ends.
    #[must_use]
    pub fn transcript(&self, keep_hypothesis: bool) -> String {
        let settled = self.settled.trim();
        if !keep_hypothesis {
            return settled.to_owned();
        }
        let hypothesis = self.hypothesis.trim();
        if hypothesis.is_empty() {
            return settled.to_owned();
        }
        if settled.is_empty() {
            return hypothesis.to_owned();
        }
        format!("{settled} {hypothesis}")
    }

    /// The live update the composer paints: settled words and the current
    /// hypothesis, split so the composer can render the tail differently.
    #[must_use]
    pub fn live_update(&self) -> LiveTranscript {
        LiveTranscript {
            settled: self.settled.trim().to_owned(),
            hypothesis: self.hypothesis.trim().to_owned(),
        }
    }

    /// Apply one event, returning what the component must do.
    ///
    /// `error` on the toggle is the caption for an engine that cannot start,
    /// which the component resolves through `engine::start_refusal` first.
    #[must_use]
    pub fn apply(&mut self, event: VoiceEvent) -> Vec<VoiceEffect> {
        match event {
            VoiceEvent::Toggle {
                active,
                claimed,
                engine_available,
            } => self.toggle(active, claimed, engine_available),
            VoiceEvent::Live => self.live(),
            VoiceEvent::Settled => self.settle(),
            VoiceEvent::Failed => self.fail(),
            VoiceEvent::Deactivated => self.force_finish(false),
            VoiceEvent::WatchdogExpired => self.force_finish(true),
            VoiceEvent::Discard => self.discard(),
        }
    }

    fn toggle(&mut self, active: bool, claimed: bool, engine_available: bool) -> Vec<VoiceEffect> {
        if !engine_available {
            // The caption is resolved by the caller, which knows the page's
            // secure-context state; a refusal never reaches the engine.
            self.state = VoiceState::Idle;
            return Vec::new();
        }
        match self.state {
            VoiceState::Idle => {
                if !active || !claimed {
                    return Vec::new();
                }
                self.state = VoiceState::Starting;
                self.settled.clear();
                self.hypothesis.clear();
                self.error = None;
                vec![
                    VoiceEffect::ClearFinalizeWatchdog,
                    VoiceEffect::ClearEngineText,
                    VoiceEffect::ClaimVoice,
                    VoiceEffect::StartEngine,
                ]
            }
            // Deliberately inert: the start is already in flight.
            VoiceState::Starting => Vec::new(),
            VoiceState::Listening => {
                self.state = VoiceState::Finalizing;
                vec![VoiceEffect::StopAndSend, VoiceEffect::ArmFinalizeWatchdog]
            }
            VoiceState::Finalizing => self.force_finish(true),
        }
    }

    fn live(&mut self) -> Vec<VoiceEffect> {
        if self.state != VoiceState::Starting {
            return Vec::new();
        }
        self.state = VoiceState::Listening;
        Vec::new()
    }

    /// The engine finished. The words are read before the reset, because the
    /// reset is what clears them.
    fn settle(&mut self) -> Vec<VoiceEffect> {
        // A settle for a mic that is not recording is one that already ended:
        // a deadline or a socket callback that outlived it. Its words have been
        // committed once already, and committing them again would append a
        // sentence the operator has read and is about to send.
        if self.state == VoiceState::Idle {
            return Vec::new();
        }
        let insert = self.transcript(true);
        let mut effects = vec![
            VoiceEffect::ClearFinalizeWatchdog,
            VoiceEffect::ClearEngineText,
        ];
        self.reset(&mut effects);
        effects.push(VoiceEffect::deliver(insert));
        effects
    }

    fn fail(&mut self) -> Vec<VoiceEffect> {
        if self.state == VoiceState::Idle {
            return Vec::new();
        }
        let mut effects = Vec::new();
        self.reset(&mut effects);
        effects.push(VoiceEffect::DiscardRecording);
        effects
    }

    fn discard(&mut self) -> Vec<VoiceEffect> {
        if self.state == VoiceState::Idle {
            return Vec::new();
        }
        let mut effects = vec![VoiceEffect::ClearFinalizeWatchdog, VoiceEffect::AbortEngine];
        self.reset(&mut effects);
        effects.push(VoiceEffect::DiscardRecording);
        effects
    }

    fn force_finish(&mut self, keep_hypothesis: bool) -> Vec<VoiceEffect> {
        if self.state == VoiceState::Idle {
            return Vec::new();
        }
        // Taken before the reset: the words are what this effect commits.
        let insert = self.transcript(keep_hypothesis);
        let mut effects = vec![VoiceEffect::ClearFinalizeWatchdog, VoiceEffect::AbortEngine];
        self.reset(&mut effects);
        effects.push(VoiceEffect::deliver(insert));
        effects
    }

    /// Back to idle: the state moves first, so an effect that re-enters the
    /// machine sees an idle mic rather than recursing into a finish.
    fn reset(&mut self, effects: &mut Vec<VoiceEffect>) {
        self.state = VoiceState::Idle;
        self.settled.clear();
        self.hypothesis.clear();
        effects.push(VoiceEffect::ReleaseVoice);
    }
}

/// The interim update the composer paints while recording.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveTranscript {
    /// Words the engine has finalized.
    pub settled: String,
    /// The current hypothesis, which may be replaced or dropped.
    pub hypothesis: String,
}

impl LiveTranscript {
    /// Whether there is anything to paint.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.settled.is_empty() && self.hypothesis.is_empty()
    }
}

#[cfg(test)]
mod tests;
