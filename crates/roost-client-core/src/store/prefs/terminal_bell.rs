//! The per-device choice of visual and audible terminal BEL surfaces.
//!
//! Stored as one stable spelling, with unknown values falling back to the
//! visual-only default so a newer value never silences an older client.

use crate::platform::KeyValueStore;
use crate::store::Store;

/// Local-storage key for the terminal bell choice.
pub const TERMINAL_BELL_KEY: &str = "roost.terminalBell";

/// Which surfaces a received BEL may use on this device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TerminalBell {
    /// Brief visual flash, the default.
    #[default]
    Visual,
    /// A short synthesized tone.
    Sound,
    /// Both visual and sound.
    VisualAndSound,
    /// No visual or audible feedback.
    Off,
}

impl TerminalBell {
    /// Stable spelling used by the settings control and local storage.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Visual => "visual",
            Self::Sound => "sound",
            Self::VisualAndSound => "both",
            Self::Off => "off",
        }
    }

    /// Parse a stored spelling, defaulting unknown values to visual.
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("sound") => Self::Sound,
            Some("both") => Self::VisualAndSound,
            Some("off") => Self::Off,
            _ => Self::Visual,
        }
    }
}

/// Set and persist the device's terminal bell choice.
pub fn set_terminal_bell(
    store: &mut Store,
    storage: &dyn KeyValueStore,
    value: TerminalBell,
) -> bool {
    if store.prefs.terminal_bell == value {
        return false;
    }
    store.prefs.terminal_bell = value;
    storage.set(TERMINAL_BELL_KEY, value.as_str());
    store.note_change();
    tracing::debug!(target: "store", preference = value.as_str(), "terminal bell preference changed");
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_storage_values_default_to_visual() {
        assert_eq!(TerminalBell::parse(None), TerminalBell::Visual);
        assert_eq!(TerminalBell::parse(Some("future")), TerminalBell::Visual);
    }
}
