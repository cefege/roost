//! Who owns the microphone. One page may show many composers — panes, the
//! viewport dock, the compact deck — and exactly one of them may hold the voice
//! slot.
//!
//! The claim is a compare-and-set on a token, which is what makes two taps in one
//! task produce exactly one open device: the second tap runs after the first has
//! already claimed, and loses. Ports `apps/web/src/voice/voiceState.ts`
//! (`activeVoiceOwner`, `claimVoice`, `releaseVoice`) without the module-level
//! signal — the global holder is `super::ownership::slot` in the browser, and
//! this type is the whole of what it holds.

/// The composer that holds the voice slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceOwner {
    /// The session whose composer owns it.
    pub session_id: String,
    /// The owner's per-mount token. Identity is the token, never the session:
    /// two composers for the same session would otherwise be indistinguishable.
    pub token: u64,
}

/// The page's single voice slot.
#[derive(Debug, Default)]
pub struct VoiceSlot {
    owner: Option<VoiceOwner>,
    next_token: u64,
}

impl VoiceSlot {
    /// An empty slot.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Take a fresh token for a newly mounted composer.
    pub fn issue_token(&mut self) -> u64 {
        self.next_token += 1;
        self.next_token
    }

    /// Try to claim the slot for one owner.
    ///
    /// `false` when another token already holds it. Claiming again with the
    /// token that already holds it succeeds, so a component re-entering
    /// `startRecording` is not a second device.
    pub fn claim(&mut self, session_id: &str, token: u64) -> bool {
        if self
            .owner
            .as_ref()
            .is_some_and(|owner| owner.token != token)
        {
            return false;
        }
        self.owner = Some(VoiceOwner {
            session_id: session_id.to_owned(),
            token,
        });
        true
    }

    /// Whether this token holds the slot.
    #[must_use]
    pub fn owns(&self, token: u64) -> bool {
        self.owner
            .as_ref()
            .is_some_and(|owner| owner.token == token)
    }

    /// Release, but only from the holder: a composer that lost the race must not
    /// clear the winner's device.
    pub fn release(&mut self, token: u64) {
        if self.owns(token) {
            self.owner = None;
        }
    }

    /// Whether a recording is open somewhere, across every state from
    /// `starting` to `finalizing`.
    #[must_use]
    pub fn is_claimed(&self) -> bool {
        self.owner.is_some()
    }
}

mod page {
    use super::VoiceSlot;
    use std::cell::RefCell;

    thread_local! {
        static SLOT: RefCell<VoiceSlot> = RefCell::new(VoiceSlot::new());
    }

    /// The page's voice slot. A composer borrows it for the length of one
    /// recording, which is why every claim names its token.
    pub fn with_slot<R>(visit: impl FnOnce(&mut VoiceSlot) -> R) -> R {
        SLOT.with(|slot| visit(&mut slot.borrow_mut()))
    }
}

pub use page::with_slot;

/// The browser's microphone-permission latch, and whether a dictation tap may
/// start without a fresh user gesture.
///
/// A shell action (a pad binding, a keyboard shortcut) can reach the mic, but a
/// browser only honours a microphone request inside a gesture, so a
/// permission-queried `granted` — or a mic still warm from a previous
/// recording — is what makes a non-gesture start legal. Ports
/// `voiceState.ts`'s `noteMicPermissionGranted` / `micStartableWithoutGesture`.
#[derive(Debug, Default)]
pub struct MicPermission {
    granted: bool,
    warm: bool,
}

impl MicPermission {
    /// Latch a granted permission. Idempotent, and a resolved non-granted probe
    /// never clears it: the Permissions API reports `prompt` before the first
    /// grant, and treating that as a refusal would gate every first tap.
    pub fn note_granted(&mut self) {
        self.granted = true;
    }

    /// Record the state a Permissions probe reported. Only `granted` sets the
    /// latch; nothing clears it.
    pub fn observe_state(&mut self, granted: bool) {
        if granted {
            self.granted = true;
        }
    }

    /// Record that a capture pipeline is currently open.
    pub fn set_warm(&mut self, warm: bool) {
        self.warm = warm;
    }

    /// Whether a dictation may start from a shell action rather than a tap.
    #[must_use]
    pub fn can_start_without_gesture(&self) -> bool {
        self.granted || self.warm
    }
}

/// What a dictation command arriving from outside the DOM does: v2's
/// `voiceControls.toggle()`, which either asks the mounted composer to toggle
/// or refuses to idle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellToggle {
    /// Toggle the mounted composer's recording.
    ToggleRecord,
    /// Refuse: the browser would not honour a microphone request that did not
    /// come from a gesture, so pretending to start would open nothing.
    FailToIdle,
}

/// Decide what a shell-raised dictation command does.
///
/// A recording already open always toggles, because stopping one is not a
/// microphone request and needs no gesture.
#[must_use]
pub fn shell_toggle(dictating: bool, can_start_without_gesture: bool) -> ShellToggle {
    if dictating || can_start_without_gesture {
        ShellToggle::ToggleRecord
    } else {
        ShellToggle::FailToIdle
    }
}

#[cfg(test)]
mod tests {
    use super::{MicPermission, ShellToggle, VoiceSlot, shell_toggle};

    #[test]
    fn the_first_claim_wins_and_a_second_token_loses() {
        let mut slot = VoiceSlot::new();
        let first = slot.issue_token();
        let second = slot.issue_token();
        assert!(slot.claim("session-a", first));
        assert!(!slot.claim("session-b", second));
        assert!(slot.owns(first));
        assert!(!slot.owns(second));
    }

    #[test]
    fn reclaiming_with_the_holding_token_is_not_a_second_device() {
        let mut slot = VoiceSlot::new();
        let token = slot.issue_token();
        assert!(slot.claim("session-a", token));
        assert!(slot.claim("session-a", token));
    }

    #[test]
    fn a_loser_releasing_does_not_clear_the_winner() {
        let mut slot = VoiceSlot::new();
        let first = slot.issue_token();
        let second = slot.issue_token();
        slot.claim("session-a", first);
        slot.release(second);
        assert!(slot.owns(first));
        assert!(slot.is_claimed());
        slot.release(first);
        assert!(!slot.is_claimed());
    }

    #[test]
    fn the_permission_latch_is_only_ever_set() {
        let mut permission = MicPermission::default();
        assert!(!permission.can_start_without_gesture());
        permission.observe_state(false);
        assert!(!permission.can_start_without_gesture());
        permission.note_granted();
        assert!(permission.can_start_without_gesture());
        permission.observe_state(false);
        assert!(permission.can_start_without_gesture());
    }

    #[test]
    fn a_warm_mic_alone_makes_a_gesture_free_start_legal() {
        let mut permission = MicPermission::default();
        permission.set_warm(true);
        assert!(permission.can_start_without_gesture());
        permission.set_warm(false);
        assert!(!permission.can_start_without_gesture());
    }

    #[test]
    fn a_shell_command_without_a_gesture_refuses_to_start_a_recording() {
        assert_eq!(shell_toggle(false, false), ShellToggle::FailToIdle);
    }

    #[test]
    fn a_latched_permission_or_a_warm_mic_lets_a_shell_command_toggle() {
        assert_eq!(shell_toggle(false, true), ShellToggle::ToggleRecord);
    }

    #[test]
    fn stopping_a_recording_never_needs_a_gesture() {
        assert_eq!(shell_toggle(true, false), ShellToggle::ToggleRecord);
    }
}
