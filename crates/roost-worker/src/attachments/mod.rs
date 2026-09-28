//! The session attachment surface: where a browser's files land, the durable
//! operation that writes them, the reaper that bounds them, the grants, hello
//! admission and lease a direct carrier runs under, and the loopback and peer
//! carriers themselves (`direct_*`, `peer_*`). Ports v2
//! `apps/worker/src/attachments/`. Composed once by `runtime::owners`; the
//! coordinator link reaches it through [`link`].

pub mod direct_chunks;
pub mod direct_frames;
pub mod direct_hello;
pub mod direct_loopback;
pub mod direct_owners;
pub mod direct_session;
pub mod direct_sockets;
pub mod file_hash;
pub mod file_store;
mod grant_checks;
mod grant_listeners;
pub mod grants;
pub mod journal;
pub mod link;
pub mod naming;
mod operation_commit;
mod operation_open;
pub mod operation_owner;
pub mod owners;
pub mod peer_budget;
pub mod peer_connection;
pub mod peer_negotiation;
pub mod peer_owner;
pub mod peer_packet_egress;
pub mod peer_packet_port;
pub mod peer_request_validation;
pub mod reaper;
pub mod receipts;
pub mod store_paths;
pub mod transfer_admission;
pub mod transfer_lease;
pub mod transfer_port;
pub mod upload;

use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};

/// The monotonic clock the grant store and the operation owner read, injected
/// so a test can move time past a grant's TTL or an operation's idle bound.
pub type AttachmentClock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// The process's own monotonic clock, for production composition.
pub fn system_clock() -> AttachmentClock {
    Arc::new(Instant::now)
}

/// Which lane carried an attachment operation. v2 `AttachmentOperationCarrier`.
///
/// It is part of an operation's identity: a coordinator relay keeps its
/// progress in memory and can never resume, while a direct operation's
/// progress is journaled before each acknowledgement, so one carrier may never
/// continue an operation the other started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Carrier {
    /// Relayed by the coordinator (`attachmentChunk`).
    Coordinator,
    /// A loopback socket or WebRTC peer port on this machine.
    Direct,
}

impl Carrier {
    /// The name a journal and a log line spell.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Coordinator => "coordinator",
            Self::Direct => "direct",
        }
    }
}

/// What an attachment operation was asked to write, before any byte arrived.
/// v2 `AttachmentOperationDescriptor`.
///
/// Every later chunk must name the same descriptor: a chunk that disagrees on
/// the session, the filename, the short-path choice or the declared total is a
/// different upload, not a continuation of this one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationDescriptor {
    pub request_id: String,
    pub session_id: String,
    pub filename: String,
    /// Answer with a `.shortcuts/pN` link instead of the file's own path.
    pub short_path: bool,
    /// The total a direct hello bound, or `None` for a coordinator relay,
    /// which never declares one.
    pub total_bytes: Option<u64>,
}
