//! Why a keeper request did not happen. Owned by `keeper_pool`, and named for
//! the caller's obligation rather than the wire's cause: a refused spawn and a
//! lost connection are both "no channel", but only one of them is the caller's
//! to retry.
//! Depends on `roost_keeper::client` for the client's own reasons — nothing here.

use roost_keeper::client_error::ClientError;

/// Why the pool could not do what it was asked.
#[derive(Debug, thiserror::Error)]
pub enum PoolError {
    /// The keeper could not be reached, or refused.
    #[error("the keeper did not do it: {0}")]
    Keeper(#[from] ClientError),

    /// Every channel id has been handed out.
    ///
    /// A distinct type rather than a keeper error because nothing was asked of
    /// the keeper: the worker is out of ids, and the only repair is a keeper
    /// that has no channels to collide with.
    #[error("no channel id is free: every id has been handed out")]
    NoChannelId,

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
