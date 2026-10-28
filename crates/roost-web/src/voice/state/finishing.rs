//! The post-recording intent shared by taps and composer submission.
//! This sibling implementation keeps `VoiceMachine`'s finish transition
//! explicit without making the browser-facing state depend on its intent.

use super::{FinishIntent, VoiceEffect, VoiceMachine, VoiceState};

impl VoiceMachine {
    /// A Send pressed while finalizing waits for the engine's answer to the
    /// stop. While live, it first stops the engine and records that same intent.
    pub(super) fn queue_send(&mut self) -> Vec<VoiceEffect> {
        match self.state {
            VoiceState::Listening => {
                self.state = VoiceState::Finalizing;
                self.finish_intent = FinishIntent::Submit;
                vec![VoiceEffect::ArmFinalizeWatchdog, VoiceEffect::StopAndSend]
            }
            VoiceState::Finalizing => {
                self.finish_intent = FinishIntent::Submit;
                Vec::new()
            }
            VoiceState::Idle | VoiceState::Starting => Vec::new(),
        }
    }
}
