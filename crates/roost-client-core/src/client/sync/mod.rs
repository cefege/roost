//! The Sync dispatch: place every frame the coordinator sends, read a close
//! for what it says, and put the client's own frames on the wire.
//!
//! The state machine is the store's. This subtree owns the transport's half:
//! a frame that is QUEUED but not yet applied, which must keep the metadata a
//! reconnecting client needs to place it; the MEANING of a close code, which is
//! four different things and not one catch-all; and the BYTES of a typed
//! `SyncCommand`, stamped with the socket the host sends it on.
//!
//! Ported from `apps/web/src/client/sync/sync-flow.ts` and the socket lifecycle
//! it is used by (`apps/web/src/store/sync.ts:102,159,220-263`). The rules and
//! the two places v2 departs from the spec's letter are in
//! `docs/phase4-client-contract.md` §7 and §11.

pub mod abort;
pub mod close;
pub mod dispatch;
pub mod encode;
pub mod frame;
pub mod link;

pub use abort::{AbortReason, SingleSyncLoop};
pub use close::{
    BACKPRESSURE_REASON, CONNECTION_REJECTION_REASON, CloseDisposition, classify_close,
    is_connection_rejection,
};
pub use dispatch::{EnqueueOutcome, SYNC_DISPATCH_QUEUE_MAX, SyncDispatch, UnplaceableFrame};
pub use encode::encode_sync_command;
pub use frame::{FrameLane, QueuedFrame};
pub use link::{InstalledLink, can_accept_sync_link, can_open_sync_link};
