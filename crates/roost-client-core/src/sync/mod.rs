//! The Sync plane: one authenticated socket, its identity, its domains, and the
//! cursor a reconnect resumes from.
//!
//! `link` owns the socket generation and the link state, `domain` owns per-domain
//! hydration and the `domain_ready` barrier, `inbound` names the frames a socket
//! delivers, and `watermark` owns the recovery cursor. None of them owns a
//! socket: the core asks for one with an `Effect` and the host reports back with
//! a `ClientEvent`.
//!
//! Contract: `protocol/spec/sync.md`. Reasons and the two places v2 departs from
//! the spec's letter are in `docs/phase4-client-contract.md` §7 and §11.

pub mod domain;
pub mod inbound;
pub mod link;
pub mod watermark;

pub use domain::DomainToken;
pub use inbound::SyncFrame;
pub use link::{
    RetainedFrame, SYNC_AUTH_REVOKED_CLOSE_CODE, SYNC_RETAINED_FRAME_MAX, SYNC_WATERMARK_KEY,
    SyncDial, SyncDomain, SyncLink, SyncState,
};
pub use watermark::RecoveryWatermark;
