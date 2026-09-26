//! Cancel tombstones for scrollback searches, so a cancel that beats its own
//! search still retires it.
//!
//! The worker keeps this ledger too (`apps/worker/src/terminal/search/
//! terminal-search-cancellation.ts`), because the worker is where a search
//! actually runs. The coordinator keeps it as well, and the reason is a gap in
//! v2: `sessionsCancelScrollbackSearch` forwards a browser-command and returns,
//! but it does not touch the pending-RPC entry the matching
//! `SessionsSearchScrollback` is still awaiting. A browser that cancels a
//! search, then lets its tab navigate, therefore leaves the coordinator waiting
//! out the full `TERMINAL_SEARCH_RPC_DEADLINE_MS` for a scan that was already
//! called off -- and the in-flight search is what the browser abandoned.
//!
//! A tombstone is one-shot: consuming it is what retires it, so a `search_id`
//! reused later by the same tab starts clean rather than inheriting a cancel
//! that was meant for a different attempt at the same id.

use std::collections::VecDeque;

use roost_protocol::terminal_search::TERMINAL_SEARCH_RPC_DEADLINE_MS;

/// A tab's cancelled search identities, and how many of them this holds.
///
/// v2's `SEARCH_CANCEL_TOMBSTONE_MAX`, for the same reason: an abandoned
/// search must not pin memory for the life of the coordinator.
pub const SEARCH_CANCEL_TOMBSTONE_MAX: usize = 128;
/// Twice the RPC deadline, so a tombstone outlives the search it was meant to
/// cancel by enough to catch a search that was still being composed.
pub const SEARCH_CANCEL_TOMBSTONE_TTL_MS: i64 = TERMINAL_SEARCH_RPC_DEADLINE_MS as i64 * 2;

/// The cancellation identity of one search: the tab that owns it, the session
/// it ran against, and the id it carries.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SearchIdentity {
    pub viewer_id: String,
    pub session_id: String,
    pub search_id: String,
}

impl SearchIdentity {
    /// Build the identity a search or a cancel is filed under.
    #[must_use]
    pub fn new(viewer_id: &str, session_id: &str, search_id: &str) -> Self {
        Self {
            viewer_id: viewer_id.to_owned(),
            session_id: session_id.to_owned(),
            search_id: search_id.to_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Tombstone {
    expires_at_ms: i64,
}

/// Bounded, expiring cancel tombstones, in insertion order.
#[derive(Debug, Default)]
pub struct ScrollbackSearchLedger {
    entries: VecDeque<(SearchIdentity, Tombstone)>,
}

impl ScrollbackSearchLedger {
    /// An empty ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a cancel. Returns false when it displaced an older tombstone,
    /// which is the only way the ledger loses anything.
    pub fn record_cancel(&mut self, identity: &SearchIdentity, now_ms: i64) -> bool {
        self.prune(now_ms);
        self.evict_oldest();
        if let Some(existing) = self
            .entries
            .iter_mut()
            .find(|(candidate, _)| candidate == identity)
        {
            existing.1 = Tombstone {
                expires_at_ms: now_ms + SEARCH_CANCEL_TOMBSTONE_TTL_MS,
            };
            return true;
        }
        self.entries.push_back((
            identity.clone(),
            Tombstone {
                expires_at_ms: now_ms + SEARCH_CANCEL_TOMBSTONE_TTL_MS,
            },
        ));
        self.entries.len() < SEARCH_CANCEL_TOMBSTONE_MAX
    }

    /// Retire a tombstone, if one is live for this identity.
    ///
    /// The removal IS the retirement: a second search under the same id after
    /// the tombstone was consumed is a new search, not a cancelled one.
    pub fn consume_cancel(&mut self, identity: &SearchIdentity, now_ms: i64) -> bool {
        self.prune(now_ms);
        let Some(position) = self
            .entries
            .iter()
            .position(|(candidate, _)| candidate == identity)
        else {
            return false;
        };
        self.entries.remove(position);
        true
    }

    /// How many tombstones are live, for a diagnostics answer.
    #[must_use]
    pub fn live_count(&self, now_ms: i64) -> usize {
        self.entries
            .iter()
            .filter(|(_, tombstone)| tombstone.expires_at_ms > now_ms)
            .count()
    }

    fn prune(&mut self, now_ms: i64) {
        while self
            .entries
            .front()
            .is_some_and(|(_, tombstone)| tombstone.expires_at_ms <= now_ms)
        {
            self.entries.pop_front();
        }
    }

    fn evict_oldest(&mut self) {
        while self.entries.len() >= SEARCH_CANCEL_TOMBSTONE_MAX {
            self.entries.pop_front();
        }
    }
}
