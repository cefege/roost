//! The WebRTC adapter's bookkeeping: which peers are open, keyed by attempt id,
//! what closing one removes, and the count the document-wide cap reads.
//!
//! Owned by `platform::peer`, which stores its browser objects here as an opaque
//! `P`. Target-independent on purpose: the counting is decided by native tests,
//! and the `wasm32` adapter only makes web-sys calls around it.

use std::collections::BTreeMap;

/// Open peers by attempt id. An event naming an attempt that is not here names
/// a dead one, and is discarded rather than applied to its replacement.
#[derive(Debug)]
pub struct PeerTable<P> {
    peers: BTreeMap<u64, P>,
}

impl<P> Default for PeerTable<P> {
    fn default() -> Self {
        Self {
            peers: BTreeMap::new(),
        }
    }
}

impl<P> PeerTable<P> {
    /// Record an opened peer. Returns the peer this attempt id held before, so
    /// the caller can close it instead of leaking it behind the new one.
    pub fn open(&mut self, attempt_id: u64, peer: P) -> Option<P> {
        let displaced = self.peers.insert(attempt_id, peer);
        tracing::debug!(
            target: "peer",
            attempt_id,
            open = self.peers.len(),
            displaced = displaced.is_some(),
            "peer recorded open"
        );
        displaced
    }

    /// The live peer for an attempt, if that attempt is still open.
    pub fn get(&self, attempt_id: u64) -> Option<&P> {
        self.peers.get(&attempt_id)
    }

    /// Forget an attempt's peer and hand it back for teardown. `None` means it
    /// was already gone, which a fault path and a page teardown both reach.
    pub fn close(&mut self, attempt_id: u64) -> Option<P> {
        let closed = self.peers.remove(&attempt_id);
        if closed.is_some() {
            tracing::debug!(
                target: "peer",
                attempt_id,
                open = self.peers.len(),
                "peer recorded closed"
            );
        }
        closed
    }

    /// How many peers are open, for the document-wide cap.
    pub fn open_count(&self) -> usize {
        self.peers.len()
    }
}

#[cfg(test)]
mod tests {
    use super::PeerTable;

    #[test]
    fn open_and_close_move_the_count_both_ways() {
        let mut table = PeerTable::default();
        assert_eq!(table.open(1, "first"), None);
        assert_eq!(table.open(2, "second"), None);
        assert_eq!(table.open_count(), 2);

        assert_eq!(table.close(1), Some("first"));
        assert_eq!(table.open_count(), 1, "closing a peer frees its slot");
        assert_eq!(table.get(1), None, "a closed attempt is not reachable");
        assert_eq!(table.get(2), Some(&"second"));

        assert_eq!(table.close(1), None, "closing twice is not an error");
        assert_eq!(table.open_count(), 1, "a repeated close frees nothing");
        assert_eq!(table.close(2), Some("second"));
        assert_eq!(table.open_count(), 0);
    }

    #[test]
    fn reopening_an_attempt_hands_back_the_peer_it_replaces() {
        let mut table = PeerTable::default();
        assert_eq!(table.open(7, "old"), None);
        assert_eq!(table.open(7, "new"), Some("old"));
        assert_eq!(table.open_count(), 1, "one attempt is one slot");
        assert_eq!(table.get(7), Some(&"new"));
    }
}
