//! Cancellation for a scrollback search, and the tombstones that make a cancel
//! which arrives BEFORE its search work. Owned by the worker, and reached only
//! from [`super::search`].
//!
//! The problem is ordering, and it is not hypothetical: a browser that scrolls
//! away abandons a search, and the abandon routinely overtakes the search on
//! the wire. Without a record of the abandon, the search that arrives second
//! starts anyway, scans a grid nobody is watching, and holds every other
//! session's PTY output for the length of a scan whose result is discarded.
//!
//! So a cancel is RECORDED, not merely acted on. The tombstone is what the
//! later search checks, and consuming it deletes it: one cancel answers one
//! search, and a search id reused afterwards is a new search rather than one
//! still under a cancel that has long since been forgotten.
//!
//! The ledger is BOUNDED and the bound is on entries, not on bytes. An
//! unbounded one is a loopback peer that never sends a search, only cancels,
//! growing this table until the worker is out of memory — and no authentication
//! stands between that peer and this table.

use std::collections::VecDeque;
use std::time::Duration;

use roost_protocol::terminal_search::TERMINAL_SEARCH_RPC_DEADLINE_MS;
use roost_protocol::wire::brand::SessionId;

/// How many cancelled searches are remembered at once.
pub const MAX_TOMBSTONES: usize = 128;

/// How long one cancellation stays answerable.
///
/// Twice the search request's own deadline. One deadline would expire the
/// tombstone while the search it was meant to stop was still inside its own
/// budget, which is the one window where arriving late is the whole point.
pub const TOMBSTONE_TTL: Duration =
    Duration::from_millis((TERMINAL_SEARCH_RPC_DEADLINE_MS * 2) as u64);

/// One remembered cancel.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Tombstone {
    /// The browser this cancel came from, the session it named, and the search
    /// it abandoned. All three, because two tabs may search one session with
    /// the same id and only the tab that cancelled may stop its own.
    owner: String,
    session: SessionId,
    search_id: String,
    expires_at_ms: u64,
}

/// The cancels that have not yet met their search.
#[derive(Debug, Default)]
pub struct Cancellations {
    live: VecDeque<Tombstone>,
}

impl Cancellations {
    pub fn new() -> Self {
        Self::default()
    }

    /// How many cancels are still waiting for their search.
    pub fn waiting(&self) -> usize {
        self.live.len()
    }

    /// Record a cancel. The oldest entry is dropped to make room, which is
    /// insertion order rather than expiry order — and the two agree, because
    /// every entry is recorded with the same lifetime.
    pub fn record(&mut self, owner: &str, session: &SessionId, search_id: &str, now_ms: u64) {
        self.expire(now_ms);
        while self.live.len() >= MAX_TOMBSTONES {
            self.live.pop_front();
        }
        self.live.push_back(Tombstone {
            owner: owner.to_owned(),
            session: session.clone(),
            search_id: search_id.to_owned(),
            expires_at_ms: now_ms.saturating_add(TOMBSTONE_TTL.as_millis() as u64),
        });
    }

    /// Whether this search was cancelled, consuming the record.
    ///
    /// Consuming rather than reading is what makes a cancel answer exactly one
    /// search. A reader would let a browser cancel once and silently abandon
    /// every later search that happened to reuse the id.
    pub fn consume(
        &mut self,
        owner: &str,
        session: &SessionId,
        search_id: &str,
        now_ms: u64,
    ) -> bool {
        self.expire(now_ms);
        let Some(index) = self.live.iter().position(|tombstone| {
            tombstone.owner == owner
                && tombstone.session == *session
                && tombstone.search_id == search_id
        }) else {
            return false;
        };
        self.live.remove(index);
        true
    }

    fn expire(&mut self, now_ms: u64) {
        while self
            .live
            .front()
            .is_some_and(|oldest| oldest.expires_at_ms <= now_ms)
        {
            self.live.pop_front();
        }
    }
}

/// The identity of one viewer's search over one thing.
///
/// `scope` is what the search is OVER and `owner` is who asked: a session for
/// a single-session search, the whole worker for a fleet-wide one. The pair is
/// what makes a cancel answer the right search — two viewers searching one
/// session are two searches, and neither may abandon the other's.
pub fn search_owner_key(scope: &str, owner: &str) -> String {
    format!("{scope}:{owner}")
}
