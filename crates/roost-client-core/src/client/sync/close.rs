//! What a close says, and the four different things to do about it.
//!
//! A close code is not a status. The coordinator sends `1013` for TWO different
//! verdicts — the connection was refused, or the application window was
//! exceeded — and the reason string beside the code is what separates them
//! (`crates/roost-coord/src/sync_ws/upgrade_admission.rs:52-53`,
//! `ack_window.rs:42-44`). Reading the code alone is how a client redials a
//! rejected connection in a tight loop, or backs off for a verdict that means
//! nothing was lost.
//!
//! The other two codes are this client's own. `4000` retires a generation that
//! stopped being served, and `4001` is the coordinator refusing the credential.
//! All four are here because one catch-all — "the socket closed, redial" — is
//! wrong for three of them, and wrong in the two directions that cost a user
//! their terminal: an immediate redial loop on a rejected credential, and a
//! backoff on a socket that was holding records this client could resume from.

use crate::client::sync::abort::AbortReason;
use crate::sync::{
    SYNC_AUTH_REVOKED_CLOSE_CODE, SYNC_BACKPRESSURE_CLOSE_CODE, SYNC_GENERATION_RETIRED_CLOSE_CODE,
};

/// The reason the coordinator closes `1013` with when the application window was
/// exceeded. Restated from `crates/roost-coord/src/sync_ws/ack_window.rs:44`,
/// which cannot be a dependency: the coordinator and the client core are
/// siblings, and a shared literal belongs to `roost-protocol` if either of them
/// grows a third sibling that needs it.
pub const BACKPRESSURE_REASON: &str = "sync backpressure";

/// The reason the coordinator closes `1013` with when the CONNECTION was
/// refused. Same code as [`BACKPRESSURE_REASON`], opposite meaning, and the
/// reason this file exists.
pub const CONNECTION_REJECTION_REASON: &str = "connection rejected";

/// What one close means, and what it costs to get wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CloseDisposition {
    /// The application window was exceeded. Records were HELD, not dropped, so
    /// the cursor still resumes everything this client has not applied.
    Backpressure,
    /// The connection was refused. No socket ever opened; there is no cursor to
    /// resume and nothing held.
    ConnectionRejected,
    /// This client retired the generation. One redial, and a 1013 from the other
    /// side must never be read as this: the coordinator cannot tell "this tab
    /// gave up on a dead worker" from "this tab went away", and a redial storm
    /// is what a misread produces.
    GenerationRetired,
    /// The coordinator refused the credential. Terminal: a redial presents the
    /// same rejected credential.
    AuthRevoked,
    /// The peer vanished without a verdict. The network's, and the only one that
    /// gets the full backoff.
    Abrupt,
}

impl CloseDisposition {
    /// A short name for the incident log.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Backpressure => "backpressure",
            Self::ConnectionRejected => "connection_rejected",
            Self::GenerationRetired => "generation_retired",
            Self::AuthRevoked => "auth_revoked",
            Self::Abrupt => "abrupt",
        }
    }

    /// Whether the dial loop reconnects without backing off.
    ///
    /// The five abort reasons bypass the backoff because the client already knows
    /// what to do; a connection rejection and an abrupt loss do not, because what
    /// they mean is exactly what the backoff is for.
    pub const fn redials_immediately(self) -> bool {
        matches!(self, Self::Backpressure | Self::GenerationRetired)
    }

    /// Whether no further dial is automatic.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::AuthRevoked)
    }

    /// Whether the records the coordinator was holding are still there, so a
    /// redial resumes from the cursor rather than starting over.
    pub const fn preserved_records(self) -> bool {
        matches!(self, Self::Backpressure)
    }

    /// The reason to record against the link, when this close implies one.
    ///
    /// Only backpressure: it is the one close that is also a statement about the
    /// flow-control window, and the reason is what tells a later reader that the
    /// socket ended on a verdict rather than on a fault.
    pub const fn abort_reason(self) -> Option<AbortReason> {
        match self {
            Self::Backpressure => Some(AbortReason::Flow),
            _ => None,
        }
    }
}

/// Read one close.
///
/// `code` is `None` when the peer vanished without a close frame at all, which
/// the browser reports as `1006`; both land in [`CloseDisposition::Abrupt`],
/// because a network that cannot complete a close handshake has told this client
/// nothing it could act on differently.
pub fn classify_close(code: Option<u16>, reason: &str) -> CloseDisposition {
    match code {
        Some(SYNC_AUTH_REVOKED_CLOSE_CODE) => CloseDisposition::AuthRevoked,
        Some(SYNC_GENERATION_RETIRED_CLOSE_CODE) => CloseDisposition::GenerationRetired,
        Some(SYNC_BACKPRESSURE_CLOSE_CODE) if reason == BACKPRESSURE_REASON => {
            CloseDisposition::Backpressure
        }
        Some(SYNC_BACKPRESSURE_CLOSE_CODE) => CloseDisposition::ConnectionRejected,
        _ => CloseDisposition::Abrupt,
    }
}

/// Whether `reason` is the coordinator's connection-rejection string.
///
/// Named separately because a host logging a close wants to distinguish "the
/// coordinator refused us" from "the coordinator closed for some other 1013", and
/// [`classify_close`] already answers that — this is for the log line, where the
/// string itself is the evidence.
pub fn is_connection_rejection(reason: &str) -> bool {
    reason == CONNECTION_REJECTION_REASON
}
