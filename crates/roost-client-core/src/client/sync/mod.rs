//! The Sync dispatch: place every frame the coordinator sends, and read a close
//! for what it says.
//!
//! The state machine is the store's. This subtree owns the transport's half, and
//! there are exactly two things a transport half owns that a state machine
//! cannot: a frame that is QUEUED but not yet applied, which must keep the
//! metadata a reconnecting client needs to place it, and the MEANING of a close
//! code, which is four different things and not one catch-all.
//!
//! Ported from `apps/web/src/client/sync/sync-flow.ts` and the socket lifecycle
//! it is used by (`apps/web/src/store/sync.ts:102,159,220-263`). The rules and
//! the two places v2 departs from the spec's letter are in
//! `docs/phase4-client-contract.md` §7 and §11.

pub mod abort;
pub mod close;
pub mod dispatch;
pub mod frame;
pub mod link;

pub use abort::{AbortReason, SingleSyncLoop};
pub use close::{
    BACKPRESSURE_REASON, CONNECTION_REJECTION_REASON, CloseDisposition, classify_close,
    is_connection_rejection,
};
pub use dispatch::{EnqueueOutcome, SYNC_DISPATCH_QUEUE_MAX, SyncDispatch, UnplaceableFrame};
pub use frame::{FrameLane, QueuedFrame};
pub use link::{InstalledLink, can_accept_sync_link, can_open_sync_link};
