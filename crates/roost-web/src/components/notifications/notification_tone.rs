//! Browser audio cues for terminal notifications.
//! A reusable audio context plays command completion and terminal bell notes.
//! Settings previews the command completion cue on user interaction.

#[cfg(target_arch = "wasm32")]
use std::cell::RefCell;

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

/// C5 marks a finished command.
pub const DONE_CUE: [ToneNote; 1] = [ToneNote {
    frequency_hz: 523.0,
    offset_s: 0.0,
    duration_s: 0.15,
    peak_gain: 0.08,
}];

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
    /// Play `notes` now. Audio is optional; refusals are logged.
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
