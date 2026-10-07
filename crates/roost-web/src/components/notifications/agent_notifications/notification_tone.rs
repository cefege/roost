//! The sound cue an agent notification plays: two short ascending tones when an
//! agent needs input, one short tone when it finished, synthesized with Web
//! Audio oscillators so there is no audio asset to ship. Called by
//! `AgentNotifications` on the same due delivery that raises a toast, and by the
//! Settings notifications pane to preview a sound switched on. Ports
//! `playCue` in `apps/web/src/components/notifications/AgentNotificationBridge.tsx`.

#[cfg(target_arch = "wasm32")]
use std::cell::RefCell;

use roost_client_core::store::prefs::notify::{NotifyPref, NotifyPrefs};

use super::scheduler::AgentNotificationKind;

/// One sine note within a cue, timed from the cue's start.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ToneNote {
    /// Pitch.
    pub frequency_hz: f32,
    /// When the note starts, after the cue starts.
    pub offset_s: f64,
    /// How long the note sounds, attack to silence.
    pub duration_s: f64,
    /// The loudest point of the envelope, as a linear gain.
    pub peak_gain: f32,
}

/// E5 then A5: a rising pair reads as a question, which is what a blocked agent
/// is asking.
const BLOCKED_CUE: [ToneNote; 2] = [
    ToneNote {
        frequency_hz: 660.0,
        offset_s: 0.0,
        duration_s: 0.12,
        peak_gain: 0.12,
    },
    ToneNote {
        frequency_hz: 880.0,
        offset_s: 0.14,
        duration_s: 0.15,
        peak_gain: 0.12,
    },
];

/// C5 alone and quieter: a finished turn is news, not a request.
const DONE_CUE: [ToneNote; 1] = [ToneNote {
    frequency_hz: 523.0,
    offset_s: 0.0,
    duration_s: 0.15,
    peak_gain: 0.08,
}];

/// The notes for `kind`.
#[must_use]
pub fn tone_for(kind: AgentNotificationKind) -> &'static [ToneNote] {
    match kind {
        AgentNotificationKind::Blocked => &BLOCKED_CUE,
        AgentNotificationKind::Done => &DONE_CUE,
    }
}

/// The switch that sounds `kind`.
#[must_use]
pub const fn sound_pref(kind: AgentNotificationKind) -> NotifyPref {
    match kind {
        AgentNotificationKind::Blocked => NotifyPref::BlockedSound,
        AgentNotificationKind::Done => NotifyPref::DoneSound,
    }
}

/// The cue a sound switch previews, or `None` for a switch that is not a sound.
#[must_use]
pub const fn previewed_kind(pref: NotifyPref) -> Option<AgentNotificationKind> {
    match pref {
        NotifyPref::BlockedSound => Some(AgentNotificationKind::Blocked),
        NotifyPref::DoneSound => Some(AgentNotificationKind::Done),
        NotifyPref::InApp
        | NotifyPref::Desktop
        | NotifyPref::TitleBadge
        | NotifyPref::CommandFinished => None,
    }
}

/// What a due delivery does in this profile. The toast and the sound are
/// independent switches on ONE event, so a reader who turned toasts off still
/// hears the cue the toast would have come with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeliverySurfaces {
    /// Raise the in-app card.
    pub toast: bool,
    /// Play the cue.
    pub sound: bool,
}

impl DeliverySurfaces {
    /// The surfaces `prefs` turn on for `kind`.
    #[must_use]
    pub fn for_kind(prefs: &NotifyPrefs, kind: AgentNotificationKind) -> Self {
        Self {
            toast: prefs.get(NotifyPref::InApp),
            sound: prefs.get(sound_pref(kind)),
        }
    }

    /// Whether the delivery shows or plays anything at all.
    #[must_use]
    pub const fn any(self) -> bool {
        self.toast || self.sound
    }
}

/// Silence for an exponential ramp, which cannot reach zero.
#[cfg(target_arch = "wasm32")]
const SILENT_GAIN: f32 = 0.001;
/// The attack, so a note does not click on.
#[cfg(target_arch = "wasm32")]
const ATTACK_S: f64 = 0.01;
/// The oscillator outlives its envelope by this much so it stops in silence.
#[cfg(target_arch = "wasm32")]
const RELEASE_TAIL_S: f64 = 0.02;

/// One audio context, created on the first cue and reused: a browser caps how
/// many a document may open, and a context per cue would reach the cap.
#[derive(Default)]
pub struct TonePlayer {
    #[cfg(target_arch = "wasm32")]
    context: RefCell<Option<web_sys::AudioContext>>,
}

impl std::fmt::Debug for TonePlayer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("TonePlayer").finish_non_exhaustive()
    }
}

#[cfg(target_arch = "wasm32")]
impl TonePlayer {
    /// Play `kind`'s cue now. Audio is an optional surface: a browser that
    /// refuses a context or a node logs and stays silent.
    pub fn play(&self, kind: AgentNotificationKind) {
        if self.play_notes(tone_for(kind)) {
            tracing::debug!(target: "notifications", ?kind, "notification tone played");
        }
    }

    /// Play `notes` now, under the same refusal rules as [`Self::play`].
    /// Returns whether every note was scheduled.
    pub fn play_notes(&self, notes: &[ToneNote]) -> bool {
        let Some(context) = self.context() else {
            return false;
        };
        // A context created before the reader interacted with the page starts
        // suspended under the autoplay policy; resuming is allowed once they have.
        if context.state() == web_sys::AudioContextState::Suspended {
            let _ = context.resume();
        }
        let start = context.current_time();
        for note in notes {
            if let Err(error) = schedule_note(&context, start, note) {
                tracing::warn!(
                    target: "notifications",
                    error = %crate::platform::device_key::describe_js(&error),
                    "notification tone refused"
                );
                return false;
            }
        }
        true
    }

    fn context(&self) -> Option<web_sys::AudioContext> {
        let mut slot = self.context.borrow_mut();
        if slot.is_none() {
            match web_sys::AudioContext::new() {
                Ok(context) => *slot = Some(context),
                Err(error) => {
                    tracing::warn!(
                        target: "notifications",
                        error = %crate::platform::device_key::describe_js(&error),
                        "notification audio context refused"
                    );
                }
            }
        }
        slot.clone()
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl TonePlayer {
    /// A native build has no audio output; the cue is a browser surface.
    pub fn play(&self, kind: AgentNotificationKind) {
        tracing::debug!(target: "notifications", ?kind, "notification tone has no audio output");
    }

    /// A native build has no audio output, so nothing is ever played.
    pub fn play_notes(&self, notes: &[ToneNote]) -> bool {
        tracing::debug!(target: "notifications", notes = notes.len(), "tone has no audio output");
        false
    }
}

/// One note: a sine oscillator through an attack/decay envelope.
#[cfg(target_arch = "wasm32")]
fn schedule_note(
    context: &web_sys::AudioContext,
    start: f64,
    note: &ToneNote,
) -> Result<(), wasm_bindgen::JsValue> {
    let oscillator = context.create_oscillator()?;
    let envelope = context.create_gain()?;
    oscillator.set_type(web_sys::OscillatorType::Sine);
    oscillator.frequency().set_value(note.frequency_hz);
    let at = start + note.offset_s;
    let level = envelope.gain();
    level.set_value_at_time(SILENT_GAIN, at)?;
    level.linear_ramp_to_value_at_time(note.peak_gain, at + ATTACK_S)?;
    level.exponential_ramp_to_value_at_time(SILENT_GAIN, at + note.duration_s)?;
    oscillator.connect_with_audio_node(&envelope)?;
    envelope.connect_with_audio_node(&context.destination())?;
    oscillator.start_with_when(at)?;
    oscillator.stop_with_when(at + note.duration_s + RELEASE_TAIL_S)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blocked_agent_rises_and_a_finished_one_sounds_once() {
        let blocked = tone_for(AgentNotificationKind::Blocked);
        assert_eq!(blocked.len(), 2);
        assert!(blocked[1].frequency_hz > blocked[0].frequency_hz);
        assert!(
            blocked[1].offset_s >= blocked[0].offset_s + blocked[0].duration_s,
            "the second tone starts after the first has decayed"
        );
        assert_eq!(tone_for(AgentNotificationKind::Done).len(), 1);
    }

    #[test]
    fn every_note_is_audible_and_short() {
        for kind in [AgentNotificationKind::Blocked, AgentNotificationKind::Done] {
            for note in tone_for(kind) {
                assert!(note.peak_gain > 0.0 && note.peak_gain <= 0.2, "{note:?}");
                assert!(note.duration_s > 0.0 && note.duration_s <= 0.3, "{note:?}");
            }
        }
    }

    #[test]
    fn the_sound_rides_the_toast_event_under_its_own_switch() {
        let prefs = NotifyPrefs {
            in_app: false,
            blocked_sound: true,
            done_sound: false,
            ..NotifyPrefs::default()
        };
        assert_eq!(
            DeliverySurfaces::for_kind(&prefs, AgentNotificationKind::Blocked),
            DeliverySurfaces {
                toast: false,
                sound: true
            }
        );
        let done = DeliverySurfaces::for_kind(&prefs, AgentNotificationKind::Done);
        assert!(!done.any());
        let defaults =
            DeliverySurfaces::for_kind(&NotifyPrefs::default(), AgentNotificationKind::Done);
        assert_eq!(
            defaults,
            DeliverySurfaces {
                toast: true,
                sound: false
            }
        );
    }

    #[test]
    fn only_the_sound_switches_preview_a_cue() {
        assert_eq!(
            previewed_kind(NotifyPref::BlockedSound),
            Some(AgentNotificationKind::Blocked)
        );
        assert_eq!(
            previewed_kind(NotifyPref::DoneSound),
            Some(AgentNotificationKind::Done)
        );
        assert_eq!(previewed_kind(NotifyPref::Desktop), None);
        for kind in [AgentNotificationKind::Blocked, AgentNotificationKind::Done] {
            assert_eq!(previewed_kind(sound_pref(kind)), Some(kind));
        }
    }
}
