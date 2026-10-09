//! Program-to-operator terminal signals, one shape for every crate: an OSC 9;4
//! progress report, an OSC 9 / OSC 777 desktop notification, and an OSC 1337
//! SetUserVar variable. `roost-term` produces them, the worker ships them in
//! `TerminalMetadata`, the coordinator retains or fans them out, and the
//! browser shows them. The wire codes live here so the two protos agree.

use serde::{Deserialize, Serialize};

/// A progress report's state, as OSC 9;4 defines them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminalProgress {
    /// State 0: no progress to show.
    Clear,
    /// State 1: a percentage.
    Normal(u8),
    /// State 2: failed, optionally at a percentage.
    Error(Option<u8>),
    /// State 3: busy with no known percentage.
    Indeterminate,
    /// State 4: paused, optionally at a percentage.
    Paused(Option<u8>),
}

impl TerminalProgress {
    /// The OSC 9;4 state code and percentage, as both protos carry them.
    pub fn to_wire(self) -> (u32, Option<u32>) {
        match self {
            Self::Clear => (0, None),
            Self::Normal(percent) => (1, Some(u32::from(percent))),
            Self::Error(percent) => (2, percent.map(u32::from)),
            Self::Indeterminate => (3, None),
            Self::Paused(percent) => (4, percent.map(u32::from)),
        }
    }

    /// The report a wire state code names; `None` for an unknown code.
    /// Percentages clamp to 100.
    pub fn from_wire(state: u32, percent: Option<u32>) -> Option<Self> {
        let percent = percent.map(|value| u8::try_from(value.min(100)).unwrap_or(100));
        Some(match state {
            0 => Self::Clear,
            1 => Self::Normal(percent.unwrap_or(0)),
            2 => Self::Error(percent),
            3 => Self::Indeterminate,
            4 => Self::Paused(percent),
            _ => return None,
        })
    }
}

/// A program's request to notify the operator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalNotification {
    /// Empty for OSC 9, which carries only a body.
    pub title: String,
    pub body: String,
}

/// One published shell variable.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TerminalUserVar {
    pub key: String,
    pub value: String,
}
