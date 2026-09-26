//! The Sync firehose: the retained frames and the per-lane meta every socket
//! egress reads, and the adapters that turn one bus message into one frame.
//!
//! One field on `CoordServices`, reached as `core.services.feed`. The seed and
//! the live path share one retained set, which is the whole reason a Sync
//! socket's snapshot/live gap is closeable.
//!
//! `new()` takes nothing and must keep taking nothing: the retention bounds
//! are read at call time from `core.services.boot`.

/// The firehose state one coordinator process holds.
#[derive(Debug, Default)]
pub struct FeedRuntime;

impl FeedRuntime {
    /// A feed that has retained no frame.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}
