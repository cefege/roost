//! Why a keeper request did not happen. Owned by `keeper_pool`, and named for
//! the caller's obligation rather than the wire's cause: a refused spawn and a
//! lost connection are both "no channel", but only one of them is the caller's
//! to retry.
//! Depends on `roost_keeper::client` for the client's own reasons and on
//! `roost_protocol`'s `ChannelId` for the one id a keeper cannot address —
//! nothing here.

use roost_keeper::client_error::ClientError;

use roost_protocol::wire::brand::ChannelId;

/// Why the pool could not do what it was asked.
#[derive(Debug, thiserror::Error)]
pub enum PoolError {
    /// The keeper could not be reached, or refused.
    #[error("the keeper did not do it: {0}")]
    Keeper(#[from] ClientError),

    /// This id belongs to a channel the keeper is already using, or to one the
    /// worker's counter has already passed.
    ///
    /// Its own variant because nothing was asked of the keeper: this worker's
    /// ids were spent elsewhere, and the repair is the CALLER's. `channel_id` is
    /// the id handed in and `highest` the mark the pool has learned. The pool
    /// will not pick a different id, because a pool that could is the second
    /// allocator this refusal exists to prevent.
    #[error(
        "channel {channel_id} is not free: this worker has already spent every id up to {highest}"
    )]
    ChannelIdTaken { channel_id: u16, highest: u16 },

    /// The id does not fit what the keeper addresses channels by.
    ///
    /// A keeper channel is a `u16` on the wire, so an id outside that range can
    /// never be spawned however free it is. It is refused rather than truncated,
    /// because truncating would address a DIFFERENT channel than the caller
    /// named.
    #[error("channel {0} does not fit the 16-bit channel ids a keeper addresses")]
    ChannelIdTooWide(ChannelId),

    /// This pool's connection is gone, so the request was never written.
    ///
    /// Its own variant because it is a decision rather than a failure: a write
    /// into a dead socket looks successful to the caller and disappears, and
    /// the caller's only honest options are to reconnect or to report the
    /// session lost.
    #[error("the keeper connection is gone: {0}")]
    Disconnected(String),

    /// The pool has no record of a channel the caller asked about.
    #[error("channel {0} is not one this worker drives")]
    Untracked(u16),
}
