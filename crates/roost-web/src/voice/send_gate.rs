//! What the composer's Send does while the mic is where it is.
//!
//! A pure decision over `super::state::VoiceState`, read by the terminal
//! composer's send button and its Enter key. A stopped recording still offers
//! Send: its words are on screen, and the press waits for the engine's answer
//! to the stop rather than being refused or sent ahead of it.

use super::state::VoiceState;

/// What one Send press does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendGate {
    /// No recording: the draft is submitted now.
    Submit,
    /// The recording was stopped and the engine's answer to the stop is still
    /// on the wire. Submitting the field now would ship a hypothesis that
    /// answer may revise and drop the words it may add, so the press is queued
    /// with the mic, which submits the draft once the answer is in it.
    AfterFinalize,
    /// A recording is opening or live: a submission would put half a sentence
    /// in the terminal, so Send is not offered and Enter stays a newline.
    Withheld,
}

impl SendGate {
    /// The gate for a mic in `state`.
    #[must_use]
    pub fn for_state(state: VoiceState) -> Self {
        match state {
            VoiceState::Idle => Self::Submit,
            VoiceState::Finalizing => Self::AfterFinalize,
            VoiceState::Starting | VoiceState::Listening => Self::Withheld,
        }
    }

    /// Whether the composer renders Send and lets Enter submit.
    #[must_use]
    pub fn offers_send(self) -> bool {
        !matches!(self, Self::Withheld)
    }
}

#[cfg(test)]
mod tests {
    use super::{SendGate, VoiceState};

    #[test]
    fn a_stopped_recording_offers_send_and_queues_the_press() {
        // Its transcript is already on screen; hiding Send until the engine
        // answered the stop left a readable sentence unsendable for seconds.
        let gate = SendGate::for_state(VoiceState::Finalizing);
        assert_eq!(gate, SendGate::AfterFinalize);
        assert!(gate.offers_send());
    }

    #[test]
    fn a_recording_that_is_opening_or_live_withholds_send() {
        for state in [VoiceState::Starting, VoiceState::Listening] {
            let gate = SendGate::for_state(state);
            assert_eq!(gate, SendGate::Withheld, "{state:?}");
            assert!(!gate.offers_send(), "{state:?}");
        }
    }

    #[test]
    fn no_recording_submits_at_once() {
        let gate = SendGate::for_state(VoiceState::Idle);
        assert_eq!(gate, SendGate::Submit);
        assert!(gate.offers_send());
    }
}
