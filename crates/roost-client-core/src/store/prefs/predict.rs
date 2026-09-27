//! The predictive echo mode, and the normalizer that makes old values mean
//! something.
//!
//! One closed set of four, and a stored string that is not one of them resolves
//! to `adaptive` — the mode that decides per keystroke rather than promising
//! either behaviour. Failing a setting closed to the adaptive mode is the whole
//! point: `"0"` and `"force"` are what two earlier builds wrote for
//! `never` and `always`, and a build that rejected them would silently reset a
//! preference the user set (`predictPref.ts:22-28`).
//!
//! The hot path never touches storage. The mode is read once into the store and
//! handed to each pane's predictor, so a keystroke is not a storage read.

use crate::platform::KeyValueStore;
use crate::store::Store;
use crate::store::prefs::PREDICT_MODE_KEY;

/// How much speculative echo the terminal paints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PredictMode {
    /// Decide per keystroke from the confidence gate. The default, and the only
    /// mode that promises nothing.
    #[default]
    Adaptive,
    /// Paint the prediction whenever the gate would allow one, without letting
    /// the gate turn it off.
    Always,
    /// Never paint one.
    Never,
    /// As `always`, plus the heuristics that are still being tuned.
    Experimental,
}

impl PredictMode {
    /// The stored spelling. The only four values ever written.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Adaptive => "adaptive",
            Self::Always => "always",
            Self::Never => "never",
            Self::Experimental => "experimental",
        }
    }
}

/// Read a stored value, normalising the two spellings earlier builds wrote.
pub fn parse(value: Option<&str>) -> PredictMode {
    match value {
        Some("0") | Some("never") => PredictMode::Never,
        Some("force") | Some("always") => PredictMode::Always,
        Some("experimental") => PredictMode::Experimental,
        _ => PredictMode::Adaptive,
    }
}

/// Change the mode, and persist.
///
/// The parameter is the raw string a settings control produced, not a
/// `PredictMode`: a control that hands this an unknown value gets `adaptive`
/// rather than having its own enum widened. Normalising on the way IN is what
/// makes the value on disk one of the four.
pub fn set_predict_mode(store: &mut Store, storage: &dyn KeyValueStore, value: &str) -> bool {
    let mode = parse(Some(value));
    if store.prefs.predict == mode {
        return false;
    }
    store.prefs.predict = mode;
    storage.set(PREDICT_MODE_KEY, mode.as_str());
    store.note_change();
    tracing::debug!(target: "store", mode = mode.as_str(), "predictive echo mode");
    true
}
