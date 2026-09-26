//! The pairing ceremony: an anonymous browser's path to becoming a device.
//!
//! One field on `CoordServices`, reached as `core.services.pairing`. The seven
//! `Pair*` methods and the scheduled retention sweep both act on this one
//! domain, so a second implementation would be a second answer to "which
//! requests are still redeemable".
//!
//! `new()` takes nothing and must keep taking nothing: anything the ceremony
//! needs from configuration is read at call time from `core.services.boot`.
//!
//! WHY THE REFUSALS ARE ONE ENUM AND NOT SEVEN `ConnectError` CONSTRUCTIONS.
//! A refusal is a decision about WHERE in the ceremony the request stopped, and
//! that decision is the security property. Written as one enum in this file it
//! is the only place a status code is chosen, it is testable with no database
//! and no Connect runtime, and no state machine below names a status at all.

use connectrpc::{ConnectError, ErrorCode};

pub mod account;
pub mod agent_table;
pub mod authority;
pub mod confirm_row;
pub mod confirmation;
pub mod provenance;
pub mod retention;
pub mod rows;
pub mod rpc_support;
pub mod secrets;
pub mod status;

pub use retention::{RetentionSweep, spawn_pair_request_retention, sweep_pair_requests};

/// The pairing state one coordinator process holds.
#[derive(Debug, Default)]
pub struct PairingRuntime;

impl PairingRuntime {
    /// An empty ceremony: no request has been created yet.
    ///
    /// The ceremony's live state is in `pair_requests`, not here, so this
    /// process holds no pairing state at all. The type survives because the
    /// retention sweep and the seven handlers have to name the domain they
    /// share, and a `CoordServices` field that is the domain's name is one
    /// place to hang whatever the domain grows next: the in-process
    /// confirmation attempt counters a second coordinator process cannot see.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// The result of every durable pairing step.
pub type PairingResult<T> = Result<T, PairingError>;

/// Every way a pairing request can be refused, and the one status each maps to.
///
/// The variants are the ceremony's stages in order, because the order is the
/// order the questions are asked in, and an operator reading a refusal needs to
/// know which question failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PairingRefusal {
    /// The browser's ceremony version is not the one this coordinator speaks.
    ///
    /// `FailedPrecondition`, not `InvalidArgument`: the request is well formed
    /// and the fix is a client reload, and a browser that retries an
    /// `InvalidArgument` unchanged would loop forever.
    #[error("pairing client must reload")]
    CeremonyVersion,
    /// `ephemeral_id` was not 32 lowercase hex characters.
    #[error("invalid pair request id")]
    InvalidRequestId,
    /// `requester_token` was not 64 lowercase hex characters.
    #[error("invalid pair requester token")]
    InvalidRequesterToken,
    /// `verification_code` was not exactly six ASCII digits.
    #[error("verification code must contain exactly six ASCII digits")]
    InvalidVerificationCode,
    /// `ssh_pubkey_b64` was neither a raw 32-byte key nor an `ssh-ed25519` blob.
    #[error("invalid ssh_pubkey_b64")]
    InvalidPublicKey,
    /// The request id and requester token together matched no row.
    ///
    /// One refusal for "no such id", "wrong token", and "an id that exists but
    /// is not yours", because a requester that could tell those apart could
    /// probe for ids (`handlers-pairing.ts:113,161`).
    #[error("not found")]
    NotFound,
    /// The request is decided, and decided rows never re-open.
    #[error("pair request is already terminal")]
    AlreadyTerminal,
    /// This id is live but belongs to a different request: another ceremony
    /// version, label, requester token, or public key.
    #[error("pair request id already exists")]
    IdAlreadyExists,
    /// The live request is not `pending`.
    #[error("pair request is not pending")]
    NotPending,
    /// Somebody already bound a different code to this request.
    #[error("pair request was already approved")]
    AlreadyApproved,
    /// The request's ten minutes ran out before this call.
    #[error("pair request expired")]
    Expired,
    /// The request is live but no confirmation is outstanding on it.
    #[error("pair request is not awaiting verification")]
    NotAwaitingVerification,
    /// No account can be named for this approval.
    #[error("pairing account is unavailable")]
    AccountUnavailable,
    /// The approver's key, device, or account stopped being authority.
    #[error("approver is no longer authorized")]
    ApproverUnauthorized,
    /// A key this ceremony would authorize is on the revocation list.
    #[error("authorized key was revoked")]
    KeyRevoked,
    /// The confirmation's authority is gone: the approver lost it, the account
    /// did, or the key now belongs to a worker or another account.
    #[error("pairing authority is no longer valid")]
    AuthorityInvalid,
    /// A device key is already a worker's identity.
    #[error("device key is already in use by a worker")]
    WorkerKeyConflict,
    /// A device key already belongs to a different account.
    #[error("device already belongs to another account")]
    OtherAccountConflict,
    /// The 32 live-request ceiling is full.
    #[error("too many live pair requests")]
    TooManyLiveRequests,
    /// This deployment fronts the coordinator and the request carried no
    /// verified front-door identity.
    #[error("pairing requires front-door sign-in")]
    FrontDoorSignInRequired,
    /// Only a request that originated on this coordinator's own host.
    #[error("on-host only")]
    OnHostOnly,
}

impl PairingRefusal {
    /// The Connect status this refusal answers with.
    ///
    /// The mapping is v2's, method by method, and it is the only one: a
    /// browser's retry logic and the audit trail both read the status, so two
    /// handlers refusing the same fact with two codes is a defect a user files
    /// as "it said expired and then asked me to try again".
    #[must_use]
    pub fn code(self) -> ErrorCode {
        match self {
            // Malformed wire values. The browser sent something this ceremony
            // cannot interpret, which no retry of the same value will fix.
            Self::InvalidRequestId
            | Self::InvalidRequesterToken
            | Self::InvalidVerificationCode
            | Self::InvalidPublicKey => ErrorCode::InvalidArgument,
            // Existence, deliberately one code for "no such id", "wrong token"
            // and "an id that exists but is not yours", so a requester cannot
            // probe for ids.
            Self::NotFound => ErrorCode::NotFound,
            // Progression refusals: the request is at the wrong stage. The
            // confirmation path's "this row is not in the verification phase at
            // all" is one of these rather than a missing row, because the
            // requester proved its token and answering NotFound would
            // contradict the Poll that just told it the row exists.
            Self::CeremonyVersion
            | Self::AlreadyTerminal
            | Self::NotPending
            | Self::AlreadyApproved
            | Self::Expired
            | Self::NotAwaitingVerification
            // No account can be named for this approval, so nothing about the
            // request can be decided. A precondition rather than a credential
            // problem: `Unauthenticated` would send the approver round a
            // sign-in loop that cannot help.
            | Self::AccountUnavailable => ErrorCode::FailedPrecondition,
            // Authority refusals. `PermissionDenied` rather than
            // `Unauthenticated` because the credential verified; what it no
            // longer proves is authority, and a client that re-authenticates in
            // a loop would never learn that.
            Self::KeyRevoked
            | Self::ApproverUnauthorized
            | Self::AuthorityInvalid
            | Self::OnHostOnly => ErrorCode::PermissionDenied,
            // A collision with a row that already exists.
            Self::IdAlreadyExists | Self::WorkerKeyConflict | Self::OtherAccountConflict => {
                ErrorCode::AlreadyExists
            }
            // A front door the deployment declares, on a request that did not
            // come through it.
            Self::FrontDoorSignInRequired => ErrorCode::Unauthenticated,
            // A capacity limit.
            Self::TooManyLiveRequests => ErrorCode::ResourceExhausted,
        }
    }

    /// The refusal as the error a handler returns.
    #[must_use]
    pub fn into_error(self) -> ConnectError {
        ConnectError::new(self.code(), self.to_string())
    }
}

/// A durable pairing failure: either a ceremony refusal, or a database fault.
///
/// Typed rather than stringly on purpose. The durable layer returns this so it
/// can be tested with no Connect runtime in the way, and the two are kept
/// DISTINCT because they mean opposite things to a browser: a refusal is a fact
/// about the ceremony the browser can act on, and a database fault is not. A
/// single error type that lost the distinction would let a handler answer "your
/// approval is no longer valid" to a broken disk.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{detail}")]
pub struct PairingError {
    /// The ceremony refusal, or `None` for a fault that is not one.
    refusal: Option<PairingRefusal>,
    /// The message a log line and an operator read.
    detail: String,
}

impl PairingError {
    /// A database or decoding fault, named by the step that failed.
    pub fn fault(detail: impl std::fmt::Display) -> Self {
        Self {
            refusal: None,
            detail: detail.to_string(),
        }
    }

    /// The ceremony refusal behind this error, or `None` for a fault.
    ///
    /// A handler that gets `None` must answer `Internal`: it is looking at
    /// something the ceremony did not decide, and inventing a ceremony answer
    /// for it would tell a browser its request is dead for a reason that has
    /// nothing to do with the request.
    #[must_use]
    pub fn refusal(&self) -> Option<PairingRefusal> {
        self.refusal
    }

    /// The error a handler returns for this failure.
    ///
    /// A fault's `detail` goes to the LOG and not to the peer. The detail names
    /// this domain's own step and whatever SQLite said, which is exactly what a
    /// 500's message should never contain and exactly what an operator needs;
    /// the client gets a fixed literal, because a `ConnectError` built here
    /// must never carry an internal error's text.
    #[must_use]
    pub fn into_error(self) -> ConnectError {
        match self.refusal {
            Some(refusal) => ConnectError::new(refusal.code(), refusal.to_string()),
            None => {
                tracing::error!(error = %self.detail, "a pairing statement failed");
                ConnectError::new(ErrorCode::Internal, PAIRING_FAULT_MESSAGE)
            }
        }
    }
}

impl From<PairingRefusal> for PairingError {
    fn from(refusal: PairingRefusal) -> Self {
        Self {
            refusal: Some(refusal),
            detail: refusal.to_string(),
        }
    }
}

impl From<sqlx::Error> for PairingError {
    fn from(error: sqlx::Error) -> Self {
        Self::fault(error)
    }
}

impl From<roost_protocol::ProtocolError> for PairingError {
    fn from(error: roost_protocol::ProtocolError) -> Self {
        Self::fault(error.reason)
    }
}

/// Refuse with the ceremony's own vocabulary, as a durable read or write error.
pub(crate) fn refuse(reason: PairingRefusal) -> PairingError {
    PairingError::from(reason)
}

/// What a caller is told when a pairing statement fails for a non-refusal
/// reason. The operator gets the detail from the log line `into_error` emits.
const PAIRING_FAULT_MESSAGE: &str = "pairing internal error";

/// A durable pairing statement that failed for a reason that is not a refusal.
/// `step` names the statement, so a log line says which one broke.
pub(crate) fn sqlx_error(step: &str, error: sqlx::Error) -> PairingError {
    PairingError::fault(format!("coord.{step}: {error}"))
}

/// The one refusal in this domain that is not a ceremony refusal: a caller
/// with no browser credential reached a method that needs one.
///
/// NOT built from a `ProtocolError`, deliberately. `ProtocolError`'s `Display`
/// is `"{field}: {reason}"`, so forwarding one puts an internal dotted path in
/// front of a browser -- `auth.principal: authentication required` -- which is a
/// different thing from the contract the spec states and the one a client
/// matches on. The message and the marker header are `auth-and-pairing.md`'s:
/// `Unauthenticated: authentication required` with `x-roost-auth-layer=device`,
/// which is what lets a browser tell "log in again" from "this method needs a
/// device credential".
#[must_use]
pub fn authentication_required() -> ConnectError {
    use crate::auth::principal::{AUTH_LAYER_DEVICE, AUTH_LAYER_HEADER};
    let mut error = ConnectError::new(ErrorCode::Unauthenticated, "authentication required");
    error.response_headers_mut().insert(
        axum::http::HeaderName::from_static(AUTH_LAYER_HEADER),
        axum::http::HeaderValue::from_static(AUTH_LAYER_DEVICE),
    );
    error
}
