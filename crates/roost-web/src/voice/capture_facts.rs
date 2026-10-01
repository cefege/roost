//! What a capture is doing, in the words the silence watch and the diagnostics
//! events read.
//!
//! Split out of `super::audio_capture` because these are facts ABOUT a capture,
//! not the capture itself: a caller reads what the graph has done so far without
//! owning it and without being able to change it.

use super::audio_capture::{
    DEFAULT_INPUT_RATE, IDLE_RELEASE_MS, TOUCH_IDLE_RELEASE_MS, state_name, with_mic,
};

/// Which graph delivered the frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CapturePath {
    /// No graph is open.
    #[default]
    None,
    /// The audio worklet, which runs off the main thread.
    Worklet,
    /// The deprecated script processor, used only when the worklet will not
    /// load on this browser.
    ScriptProcessor,
}

impl CapturePath {
    /// The diagnostics value.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Worklet => "worklet",
            Self::ScriptProcessor => "scriptprocessor",
        }
    }
}

/// What the silence watch and the diagnostics events need to describe a capture.
#[derive(Debug, Clone, PartialEq)]
pub struct CaptureFacts {
    /// Chunks delivered to the sink since the pipeline opened.
    pub frames: usize,
    /// The loudest sample seen, so a silent device is distinguishable from a
    /// quiet room.
    pub peak: f32,
    /// Which graph delivered them.
    pub path: CapturePath,
    /// The context's own state string.
    pub context_state: String,
    /// The rate the graph runs at.
    pub sample_rate: u32,
}

/// The idle release this device wants.
#[must_use]
pub fn idle_release_ms() -> i32 {
    if crate::components::deck::deck_dom::is_touch_device() {
        TOUCH_IDLE_RELEASE_MS
    } else {
        IDLE_RELEASE_MS
    }
}

/// Whether a graph is open right now. A latched microphone permission OR a warm
/// graph is what makes a dictation command from a pad binding legal.
#[must_use]
pub fn is_warm() -> bool {
    with_mic(|mic| mic.path.get() != CapturePath::None)
}

/// The facts the silence watch and the diagnostics events read.
#[must_use]
pub fn capture_stats() -> CaptureFacts {
    with_mic(|mic| CaptureFacts {
        frames: mic.frames.get(),
        peak: mic.peak.get(),
        path: mic.path.get(),
        context_state: mic.context.as_ref().map_or_else(
            || "none".to_owned(),
            |context| state_name(context.state()).to_owned(),
        ),
        sample_rate: mic
            .context
            .as_ref()
            .map_or(DEFAULT_INPUT_RATE, |context| context.sample_rate() as u32),
    })
}
