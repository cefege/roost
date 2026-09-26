//! The session attachment surface: the on-disk store a browser's files land in,
//! the direct loopback socket they may arrive over, and the peer negotiation
//! that carries them between machines. Depends on `roost_protocol` for the
//! transfer wire — and on nothing here.
//!
//! UPLOAD ADMISSION IS NOT HERE. `crate::attachment_transfer` already owns the
//! lease, the chunk-in-flight bound and the expiry-versus-revocation split, and
//! that rule ("an admitted upload runs to completion; a withdrawn grant stops
//! it at once") is the one worth having exactly one of. This module owns what
//! surrounds it: the operation's durable identity, its receipts, and the two
//! carriers that can deliver one.

/// Which lane carried an attachment operation.
///
/// A status answer has to name it, because the two lanes have different failure
/// modes and a client retries one and not the other: a direct socket that
/// closed mid-upload is resumable by re-dialing, while a coordinator-mediated
/// operation resumes against the journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Carrier {
    /// Relayed by the coordinator on this session's behalf.
    Coordinator,
    /// Carried over the door's loopback socket, on this machine.
    Direct,
}

impl Carrier {
    /// The wire name a status or receipt spells.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Coordinator => "coordinator",
            Self::Direct => "direct",
        }
    }
}

/// What an attachment operation was asked to do, before any byte arrived.
///
/// The descriptor is the part a client can re-send unchanged: a resumed upload
/// proves it is the same operation by naming the same request, so the request
/// id and the total are the identity and the filename is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationDescriptor {
    pub request_id: String,
    pub session_id: String,
    pub filename: String,
    /// Whether the client's path was a short one the store must resolve to its
    /// canonical name before writing.
    pub short_path: bool,
    /// The total the client expects, or `None` when it did not say and the
    /// operation is length-agnostic.
    pub total_bytes: Option<u64>,
}
