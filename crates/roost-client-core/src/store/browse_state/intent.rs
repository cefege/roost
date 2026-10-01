//! The folder browser's user actions as one event the pump dispatches: open a
//! machine's browser, move it, filter it, and ask it for the directory it is
//! standing in. The state it writes is [`super::BrowseState`]; the path
//! arithmetic it needs was already resolved by the HOST, because the canonical
//! path codec lives in `roost-platform` and the crate DAG forbids this crate
//! from depending on it.
//!
//! `Refresh` is the one intent that answers something: the caller needs the
//! [`BrowseListingRequest`] to issue, and the per-machine generation that fences
//! the reply is private to `browse_machine.rs`. Returning the request here is
//! what lets the whole move be one write — a host that set the loading state
//! and then asked for the request separately would hold a generation another
//! reader could have moved past.

use super::BrowseState;
use crate::store::Store;
use crate::store::browse_machine::BrowseListingRequest;

/// A folder-browser action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowseIntent {
    /// Open `worker_fp`'s browser, or return the one already open. `home` is
    /// used only the first time.
    Open {
        /// The machine.
        worker_fp: String,
        /// The directory it opens on, when the caller knows one.
        home: Option<String>,
    },
    /// Move one machine to `path` and record the move in its history.
    Navigate {
        /// The machine.
        worker_fp: String,
        /// The canonical path, as the host's codec spelled it.
        path: String,
    },
    /// Filter one machine's list.
    SetFilter {
        /// The machine.
        worker_fp: String,
        /// The filter text.
        filter: String,
    },
    /// Show or hide one machine's filter box.
    SetFilterOpen {
        /// The machine.
        worker_fp: String,
        /// Whether the box is showing.
        open: bool,
    },
    /// Hide the filter box and clear the text.
    ///
    /// One intent rather than the pair: a hidden filter that still hides
    /// entries is the "where did my folders go" report, and a caller that
    /// dispatches the two separately has a frame in between.
    CloseFilter {
        /// The machine.
        worker_fp: String,
    },
    /// Move one machine's history cursor back.
    GoBack {
        /// The machine.
        worker_fp: String,
    },
    /// Move one machine's history cursor forward.
    GoForward {
        /// The machine.
        worker_fp: String,
    },
    /// Forget one machine's browser, for a machine that has left the registry.
    Forget {
        /// The machine.
        worker_fp: String,
    },
    /// Ask one machine for the directory it is standing in. Answers the request
    /// to issue; `None` when the machine has no browser open.
    Refresh {
        /// The machine.
        worker_fp: String,
    },
}

impl BrowseIntent {
    /// The action's name, for the store's log line.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Open { .. } => "open",
            Self::Navigate { .. } => "navigate",
            Self::SetFilter { .. } => "set_filter",
            Self::SetFilterOpen { .. } => "set_filter_open",
            Self::CloseFilter { .. } => "close_filter",
            Self::GoBack { .. } => "go_back",
            Self::GoForward { .. } => "go_forward",
            Self::Forget { .. } => "forget",
            Self::Refresh { .. } => "refresh",
        }
    }
}

/// The machine one intent addresses, whatever else it carries.
#[must_use]
pub fn intent_worker_fp(intent: &BrowseIntent) -> &str {
    match intent {
        BrowseIntent::Open { worker_fp, .. }
        | BrowseIntent::Navigate { worker_fp, .. }
        | BrowseIntent::SetFilter { worker_fp, .. }
        | BrowseIntent::SetFilterOpen { worker_fp, .. }
        | BrowseIntent::CloseFilter { worker_fp }
        | BrowseIntent::GoBack { worker_fp }
        | BrowseIntent::GoForward { worker_fp }
        | BrowseIntent::Forget { worker_fp }
        | BrowseIntent::Refresh { worker_fp } => worker_fp,
    }
}

/// Apply one folder-browser action, answering the listing request `Refresh`
/// issued. Returns whether anything moved, so the caller knows if a revision is
/// owed.
pub fn apply_browse_intent(
    store: &mut Store,
    intent: &BrowseIntent,
) -> (bool, Option<BrowseListingRequest>) {
    let Some(machine) = roost_protocol::wire::WorkerFp::try_from(intent_worker_fp(intent)).ok()
    else {
        return (false, None);
    };
    let browse: &mut BrowseState = &mut store.browse;
    match intent {
        BrowseIntent::Open { home, .. } => {
            browse.open(&machine, home.as_deref());
            (true, None)
        }
        BrowseIntent::Navigate { path, .. } => (browse.set_cwd(&machine, path), None),
        BrowseIntent::SetFilter { filter, .. } => {
            browse.set_filter(&machine, filter);
            (true, None)
        }
        BrowseIntent::SetFilterOpen { open, .. } => {
            browse.set_filter_open(&machine, *open);
            (true, None)
        }
        BrowseIntent::CloseFilter { .. } => {
            browse.close_filter(&machine);
            (true, None)
        }
        BrowseIntent::GoBack { .. } => (browse.go_back(&machine), None),
        BrowseIntent::GoForward { .. } => (browse.go_forward(&machine), None),
        BrowseIntent::Forget { .. } => (browse.forget(&machine), None),
        BrowseIntent::Refresh { .. } => {
            let request = browse.begin_listing(&machine);
            (request.is_some(), request)
        }
    }
}
