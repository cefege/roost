//! Terminal bells received over Sync: the rings the browser has yet to present
//! (a pane flash, a tone) and the sessions that rang while nobody was looking.
//! The Sync fold calls `ring`; the browser drains `take_rings` once per ring and
//! clears a session's mark through `ShellIntent::ClearTerminalBell` once it is
//! on screen. A ring is an event, so nothing here survives a reload.

use std::collections::{BTreeSet, VecDeque};

/// Undrained rings kept at most: a tab in the background must not grow without
/// bound, and a flash for a ring minutes old means nothing.
pub const PENDING_RING_CAP: usize = 64;

/// Bells not yet presented, and sessions with an unseen ring.
#[derive(Debug, Default)]
pub struct TerminalBells {
    rings: VecDeque<String>,
    unseen: BTreeSet<String>,
}

impl TerminalBells {
    /// Record a ring for `session_id`: queued for presentation, and marked
    /// unseen until the browser says the session is on screen.
    pub fn ring(&mut self, session_id: &str) {
        if self.rings.len() == PENDING_RING_CAP {
            self.rings.pop_front();
        }
        self.rings.push_back(session_id.to_owned());
        self.unseen.insert(session_id.to_owned());
        tracing::debug!(target: "notifications", session_id, "terminal bell received");
    }

    /// The rings not yet presented, oldest first, removed from the queue.
    pub fn take_rings(&mut self) -> Vec<String> {
        self.rings.drain(..).collect()
    }

    /// Whether `session_id` rang since it was last on screen.
    pub fn is_unseen(&self, session_id: &str) -> bool {
        self.unseen.contains(session_id)
    }

    /// Every session with an unseen ring, in id order.
    pub fn unseen(&self) -> impl Iterator<Item = &str> {
        self.unseen.iter().map(String::as_str)
    }

    /// Drop `session_id`'s unseen mark. Returns whether one was held, which is
    /// whether the store changed.
    pub fn clear(&mut self, session_id: &str) -> bool {
        let cleared = self.unseen.remove(session_id);
        if cleared {
            tracing::debug!(target: "notifications", session_id, "terminal bell seen");
        }
        cleared
    }
}

#[cfg(test)]
mod tests {
    use super::{PENDING_RING_CAP, TerminalBells};

    #[test]
    fn a_ring_is_presented_once_and_marked_until_seen() {
        let mut bells = TerminalBells::default();
        bells.ring("s1");
        bells.ring("s1");
        assert_eq!(bells.take_rings(), ["s1", "s1"]);
        assert!(bells.take_rings().is_empty());
        assert!(bells.is_unseen("s1"));
        assert!(bells.clear("s1"));
        assert!(!bells.is_unseen("s1"));
        assert!(!bells.clear("s1"), "a second clear changes nothing");
    }

    #[test]
    fn undrained_rings_keep_only_the_newest() {
        let mut bells = TerminalBells::default();
        for index in 0..=PENDING_RING_CAP {
            bells.ring(&format!("s{index}"));
        }
        let rings = bells.take_rings();
        assert_eq!(rings.len(), PENDING_RING_CAP);
        assert_eq!(rings.first().map(String::as_str), Some("s1"));
    }
}
