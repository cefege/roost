//! What the composer's Send does while the mic is where it is.
//!
//! A pure decision over `super::state::VoiceState`, read by each composer for
//! its Send button and Enter key. A live or stopped recording queues Send with
//! the mic, which submits only after the final transcript has landed.

use super::state::VoiceState;

/// What one Send press does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendGate {
    /// No recording: the draft is submitted now.
    Submit,
    /// The recording is live or stopped and its final words are still pending.
    /// Send is queued with the mic and runs after its transcript is committed.
    AfterFinalize,
    /// A recording is opening: Send is withheld until the mic is live.
    Withheld,
}

impl SendGate {
    /// The gate for a mic in `state`.
    #[must_use]
    pub fn for_state(state: VoiceState) -> Self {
        match state {
            VoiceState::Idle => Self::Submit,
            VoiceState::Finalizing | VoiceState::Listening => Self::AfterFinalize,
            VoiceState::Starting => Self::Withheld,
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
    fn a_live_or_stopped_recording_offers_send_and_queues_the_press() {
        for state in [VoiceState::Listening, VoiceState::Finalizing] {
            let gate = SendGate::for_state(state);
            assert_eq!(gate, SendGate::AfterFinalize, "{state:?}");
            assert!(gate.offers_send(), "{state:?}");
        }
    }

    #[test]
    fn a_recording_that_is_opening_withholds_send() {
        let gate = SendGate::for_state(VoiceState::Starting);
        assert_eq!(gate, SendGate::Withheld);
        assert!(!gate.offers_send());
    }

    #[test]
    fn no_recording_submits_at_once() {
        let gate = SendGate::for_state(VoiceState::Idle);
        assert_eq!(gate, SendGate::Submit);
        assert!(gate.offers_send());
    }
}
