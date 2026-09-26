//! Retained claims: what a view record leaves behind when it is released, so
//! the same tab on the same device can reclaim its handle instead of colliding
//! with a fresh admission.
//!
//! Split out of `record.rs` for the size cap. The store is capped per viewer
//! and per process and evicted oldest-first, which means a very stale client's
//! revision guard CAN be evicted -- the guard is a tie-breaker between two
//! declarations of one tab, not a durable record.

use std::collections::{HashMap, VecDeque};

use roost_protocol::viewport::TERMINAL_VIEW_LEASE_MS;

use super::record::ViewIntent;

/// Tombstones kept per viewer, so one noisy device cannot evict everyone
/// else's reclaim state.
pub const VIEWER_TOMBSTONE_CAP: usize = 128;

/// Tombstones kept in the process.
pub const PROCESS_TOMBSTONE_CAP: usize = 131_072;

/// A retained claim for a view whose record is gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tombstone {
    /// The composite key the record was stored under.
    pub key: String,
    /// The viewer whose claim this preserves.
    pub viewer_key: String,
    /// The highest revision the record reached.
    pub revision: u64,
    /// What the record last declared.
    pub intent: ViewIntent,
    /// When this tombstone stops being reclaimable.
    pub expires_ms: u64,
    /// Retention order, so eviction is oldest-first without a second map.
    sequence: u64,
}

/// The tombstone store: a lookup map plus the recency order eviction walks.
///
/// The order list holds one entry per retention, including superseded ones; an
/// entry whose sequence no longer matches is stale and is dropped when the walk
/// reaches it, which keeps retention O(1) and eviction exact.
#[derive(Debug, Default)]
pub struct TombstoneStore {
    entries: HashMap<String, Tombstone>,
    order: VecDeque<(String, u64)>,
    next_sequence: u64,
}

impl TombstoneStore {
    /// A store with no retained claims.
    #[must_use]
    #[allow(dead_code)] // split residue: the constructor's only caller moved modules
    pub fn new() -> Self {
        Self::default()
    }

    /// The retained claim under `key`.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Tombstone> {
        self.entries.get(key)
    }

    /// How many claims are retained.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Forget one claim.
    pub fn remove(&mut self, key: &str) {
        self.entries.remove(key);
    }

    /// Drop every claim a viewer holds, and every claim whose viewer key is
    /// one of its tabs. A revoked device must not keep a claim that a future
    /// socket with the same fingerprint could replay into membership.
    pub fn remove_viewer(&mut self, fingerprint: &str) {
        let owned = format!("{fingerprint}:");
        let doomed: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, tombstone)| {
                tombstone.viewer_key == fingerprint || tombstone.viewer_key.starts_with(&owned)
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in doomed {
            self.entries.remove(&key);
        }
    }

    /// Drop every claim that names this session.
    pub fn remove_session(&mut self, session_id: &str) {
        let doomed: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, tombstone)| tombstone.intent.session_id == session_id)
            .map(|(key, _)| key.clone())
            .collect();
        for key in doomed {
            self.entries.remove(&key);
        }
    }

    /// Drop every claim whose lease has lapsed.
    pub fn expire(&mut self, now_ms: u64) {
        let doomed: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, tombstone)| tombstone.expires_ms <= now_ms)
            .map(|(key, _)| key.clone())
            .collect();
        for key in doomed {
            self.entries.remove(&key);
        }
    }

    /// Retain one claim for one lease, evicting oldest-first within the
    /// viewer's own cap and then the process cap.
    pub fn retain(
        &mut self,
        now_ms: u64,
        key: String,
        viewer_key: String,
        revision: u64,
        intent: ViewIntent,
    ) {
        self.next_sequence += 1;
        let sequence = self.next_sequence;
        self.entries.insert(
            key.clone(),
            Tombstone {
                key: key.clone(),
                viewer_key: viewer_key.clone(),
                revision,
                intent,
                expires_ms: now_ms.saturating_add(TERMINAL_VIEW_LEASE_MS),
                sequence,
            },
        );
        self.order.push_back((key, sequence));
        self.evict(&viewer_key);
    }

    fn evict(&mut self, viewer_key: &str) {
        while self.entries.len() > PROCESS_TOMBSTONE_CAP {
            if !self.evict_oldest(|_| true) {
                break;
            }
        }
        let mut own = self
            .entries
            .values()
            .filter(|tombstone| tombstone.viewer_key == viewer_key)
            .count();
        while own > VIEWER_TOMBSTONE_CAP && self.evict_oldest(|t| t.viewer_key == viewer_key) {
            own -= 1;
        }
    }

    /// Remove the oldest live claim `wanted` accepts, skipping the superseded
    /// entries retention left in the order list.
    fn evict_oldest(&mut self, wanted: impl Fn(&Tombstone) -> bool) -> bool {
        while let Some((key, sequence)) = self.order.pop_front() {
            let Some(tombstone) = self.entries.get(&key) else {
                continue;
            };
            if tombstone.sequence != sequence {
                continue;
            }
            if !wanted(tombstone) {
                // Put it back: it is the oldest entry overall, so it is also
                // the oldest the next walk would reach.
                self.order.push_front((key, sequence));
                return false;
            }
            self.entries.remove(&key);
            return true;
        }
        false
    }
}
