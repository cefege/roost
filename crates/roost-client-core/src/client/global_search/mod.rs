//! Search on the client: the coordinator's answers, the reconciliation a
//! cursor-paged answer needs, and the state machine that asks for them.
//!
//! Nothing here indexes anything. Both files read the COORDINATOR's search
//! ledger, because it is the only party that knows every machine's retained
//! history; a client-side index would answer for the machines this browser
//! happens to be connected to and call that the fleet.
//!
//! The rows themselves live in `crate::search::global`, beside the per-session
//! `SearchPage` and `FindMatch` this module's per-session sibling already owns:
//! they are the same kind of value, and the module that HOLDS rows is not the
//! module that MERGES them.

mod reconcile;
pub mod global_search;

pub use reconcile::{
    merge_global_search_matches, reconcile_global_search_partials, retain_joinable_matches,
};
pub use global_search::{
    GLOBAL_SEARCH_DEBOUNCE_MS, GlobalSearchController, GlobalSearchQuery, GlobalSearchRequest,
    GlobalSearchResults, OutstandingPage, SetSearchOutcome,
};
