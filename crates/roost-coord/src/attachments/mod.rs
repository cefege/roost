//! The attachments domain: the file RPCs, the chunk relay, the direct grants
//! and their exact tab/worker fence, and the peer negotiations.
//!
//! One field on `CoordServices`, reached as `core.services.attachments`. A
//! grant and the status call that retires it must see one table, or a grant
//! could be retired by a coordinator that never issued it.
//!
//! `new()` takes nothing and must keep taking nothing: anything the domain
//! needs from configuration is read at call time from `core.services.boot`.

/// The attachment state one coordinator process holds.
#[derive(Debug, Default)]
pub struct AttachmentsRuntime;

impl AttachmentsRuntime {
    /// A coordinator with no grants and no transfers.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}
