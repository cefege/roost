//! The bounded warm set: which parked terminals the deck keeps mounted beside
//! the ones it paints. Held by the web deck component and advanced whenever the
//! slotted sessions or the open set move. Pure; ports
//! `apps/web/src/lib/deckWarmSet.ts`.
//!
//! A parked pane keeps its renderer and scrollback, so switching back is a
//! visibility flip rather than a remount. It also stays laid out (a parked pane
//! must size truthfully), so every warm pane adds layout to every later switch;
//! the cap trades one cold remount of a long-unvisited pane for a fixed ceiling.

use std::collections::BTreeSet;

/// Warm panes kept beyond the ones the deck shows: the keyboard-reachable tab
/// range, so no tab shortcut lands on a cold mount.
pub const DECK_WARM_LIMIT: usize = 8;

/// Warm session ids in recency order, least recently slotted first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WarmSet {
    ids: Vec<String>,
}

impl WarmSet {
    /// Nothing warm.
    pub fn new() -> Self {
        Self::default()
    }

    /// The warm ids, least recently slotted first.
    pub fn ids(&self) -> &[String] {
        &self.ids
    }

    /// Whether `session_id` is warm.
    pub fn contains(&self, session_id: &str) -> bool {
        self.ids.iter().any(|id| id == session_id)
    }

    /// How many ids are warm.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Whether nothing is warm.
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Advance to the next warm set, returning whether membership OR order
    /// moved. An unchanged answer is what lets the host skip a re-render: an
    /// unrelated layout commit must not re-style every mounted pane.
    ///
    /// Slotted ids move to the newest end, so the front is the true least
    /// recently shown pane; closed ids leave and cost the survivors nothing;
    /// only the non-slotted tail is capped, so a layout showing more panes than
    /// `limit` still keeps every painted pane mounted.
    pub fn advance(
        &mut self,
        open_ids: &BTreeSet<String>,
        slotted_ids: &[String],
        limit: usize,
    ) -> bool {
        // A slot naming a session that is not open is stale: it can never
        // stream again, so holding it warm is a mount for nothing.
        let mut slotted: Vec<&String> = Vec::new();
        for id in slotted_ids {
            if open_ids.contains(id) && !slotted.contains(&id) {
                slotted.push(id);
            }
        }
        let mut next: Vec<String> = self
            .ids
            .iter()
            .filter(|id| open_ids.contains(*id) && !slotted.contains(id))
            .cloned()
            .collect();
        let evictable = next.len().saturating_sub(limit);
        next.drain(..evictable);
        next.extend(slotted.into_iter().cloned());
        if next == self.ids {
            return false;
        }
        tracing::debug!(
            target: "deck",
            warm = next.len(),
            evicted = evictable,
            "warm set advanced"
        );
        self.ids = next;
        true
    }
}
