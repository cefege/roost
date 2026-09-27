//! The one fact a keeper pool needs about channel ids: how high the keeper has
//! been seen to hold. `keeper_pool::pool` carries one of these; `session::spawn`
//! asks the pool for a PTY and is refused an id this one already covers.
//! Depends on `std::sync::atomic` and nothing else.
//!
//! WHY IT KNOWS AND DOES NOT DECIDE. The counter that actually mints ids is
//! `session::lifecycle::SessionManager`'s, and it is advanced past the keeper's
//! own maximum after an adoption (`strays::ChannelAllocator::advance_past_keeper`)
//! — a keeper outlives the worker, so a fresh worker starting at one collides
//! with an orphaned PTY, and a colliding spawn is answered `channel_id in use`
//! and the new terminal simply fails. This type mints nothing. It remembers the
//! highest id the keeper has reported or this pool has adopted, and REFUSES an id
//! at or below it, which is the one thing a caller that skipped the allocator
//! cannot be allowed to do.
//!
//! IT IS NOT THE COUNTER. A refused id here is a bug in a CALLER's allocation,
//! and the repair is that caller's: the pool has no way to pick a different one
//! without becoming the second allocator this type exists to make impossible.

use std::sync::atomic::{AtomicU16, Ordering};

/// The highest channel id the keeper has been seen to hold, and the refusal
/// that follows from it.
///
/// A single atomic rather than a table of the keeper's live ids: the only
/// question `spawn` asks is "is this id at or below something the keeper
/// already holds?", and the answer is the maximum. Keeping the whole list would
/// be a second copy of `keeper_channels`, free to disagree with the keeper.
#[derive(Debug, Default)]
pub struct ChannelIds {
    /// Zero is the honest floor: a keeper that has never been asked holds
    /// nothing, and a pool that has adopted nothing has seen nothing.
    highest: AtomicU16,
}

impl ChannelIds {
    /// A guard that has not yet been told about any channel.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an id the keeper is known to hold. Returns whether it raised the
    /// mark, so a caller can log the reconcile rather than silently having done
    /// it.
    pub fn note(&self, channel_id: u16) -> bool {
        let previous = self.highest.fetch_max(channel_id, Ordering::AcqRel);
        if previous < channel_id {
            tracing::debug!(
                %channel_id,
                previous,
                "a channel the keeper holds raised this pool's id mark"
            );
            true
        } else {
            false
        }
    }

    /// The highest id known to belong to the keeper.
    pub fn highest(&self) -> u16 {
        self.highest.load(Ordering::Acquire)
    }

    /// Whether this id is one the keeper is known to hold, or one the counter
    /// has already passed.
    ///
    /// `<=` and not `<`: the keeper holding id 8 means 8 is not free AND that
    /// every id below it is spent as well, because a counter that had handed
    /// out 8 has passed all seven.
    pub fn refuses(&self, channel_id: u16) -> bool {
        channel_id <= self.highest()
    }
}
