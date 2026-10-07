//! The wire-level mouse tracking mode used by terminal frames.
//!
//! Mouse tracking is normalized at the protocol boundary so clients share the
//! worker's known modes while retaining unknown wire values for round trips.

/// Mouse reporting mode the foreground application requested: 0 = none, 1000 =
/// press/release, 1002 = press/release plus motion while held. The core folds
/// legacy mode 9 and any-motion 1003 into these three, so no other value is
/// representable — but the wire is a raw integer, so one still has to decode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MouseTracking {
    /// No tracking requested, so the browser keeps native selection and scroll.
    #[default]
    None,
    /// DECSET 1000: press and release.
    PressRelease,
    /// DECSET 1002: press and release, plus motion while a button is held.
    ButtonMotion,
    /// A mode this build does not name, carried verbatim so it round-trips
    /// instead of failing to decode.
    Unknown(u32),
}

impl From<MouseTracking> for u32 {
    fn from(mode: MouseTracking) -> Self {
        match mode {
            MouseTracking::None => 0,
            MouseTracking::PressRelease => 1000,
            MouseTracking::ButtonMotion => 1002,
            MouseTracking::Unknown(raw) => raw,
        }
    }
}

impl From<u32> for MouseTracking {
    fn from(raw: u32) -> Self {
        match raw {
            0 => Self::None,
            1000 => Self::PressRelease,
            1002 => Self::ButtonMotion,
            raw => Self::Unknown(raw),
        }
    }
}

/// Narrow a wire integer to a MouseTracking. An unknown value reads as "no
/// tracking requested", the safe answer for a mode neither side agreed on.
pub fn as_mouse_tracking(raw: u32) -> MouseTracking {
    match MouseTracking::from(raw) {
        MouseTracking::Unknown(_) => MouseTracking::None,
        mode => mode,
    }
}
