//! The pairing ceremony's request and response bodies.
//!
//! Plain typed structs with the wire's own field names, not `roost_proto`
//! messages, because this crate's public API names the two cell wire messages
//! and nothing else: the client core describes what it wants and the host
//! encodes it, so the Connect layer keeps exactly one place that knows a
//! protobuf field number exists.
//!
//! The `PairCreate` echo is the shape worth reading twice. It carries one
//! string, and the ceremony refuses it unless it equals the request id this
//! browser generated — the coordinator answers `NotFound` generically for a
//! request it cannot see, so a matching echo is the only evidence the request
//! actually exists.
//!
//! Ported from `protocol/proto/roost/v1/coordinator.proto:718-765`; the contract
//! is `protocol/spec/auth-and-pairing.md:26-29`.

/// `PairCreate`: ask for a request bound to this browser's public key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairCreateRequest {
    /// This device's public key, standard base64.
    pub ssh_pubkey_b64: String,
    /// What the approver will see.
    pub label: String,
    /// [`PAIRING_CEREMONY_VERSION`].
    pub ceremony_version: u32,
    /// The ceremony's request id.
    pub ephemeral_id: String,
    /// The ceremony's requester token.
    pub requester_token: String,
}

/// What `PairCreate` answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairCreateResponse {
    /// The request the coordinator actually created.
    ///
    /// Checked against the id this browser generated. A mismatch is a refusal:
    /// the coordinator answers `NotFound` generically for a request it cannot
    /// see, so a matching echo is the only evidence the request exists.
    pub ephemeral_id: String,
}

/// `PairPoll`: ask what became of a request this browser owns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairPollRequest {
    /// The ceremony's request id.
    pub ephemeral_id: String,
    /// The ceremony's requester token.
    pub requester_token: String,
    /// [`PAIRING_CEREMONY_VERSION`].
    pub ceremony_version: u32,
}

/// What `PairPoll` answered: a stage name and when the request expires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairPollResponse {
    /// One of the six wire status names.
    pub status: String,
    /// When the request expires, in milliseconds.
    pub expires_at_ms: u64,
}

/// `PairConfirm`: bind the code the approver chose to this request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairConfirmRequest {
    /// The ceremony's request id.
    pub ephemeral_id: String,
    /// The ceremony's requester token.
    pub requester_token: String,
    /// Exactly six digits, already normalised.
    pub verification_code: String,
    /// [`PAIRING_CEREMONY_VERSION`].
    pub ceremony_version: u32,
}

/// What `PairConfirm` answered. `ok: false` is a wrong code, not an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PairConfirmResponse {
    /// Whether the code matched.
    pub ok: bool,
}

/// `PairDeny`: an approver refusing a request they are looking at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairDenyRequest {
    /// The request to refuse.
    pub ephemeral_id: String,
}

/// `PairApprove`: an approver binding a code to a pending request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairApproveRequest {
    /// The request being approved.
    pub ephemeral_id: String,
    /// [`PAIRING_CEREMONY_VERSION`].
    pub ceremony_version: u32,
    /// Exactly six digits, generated here and shown to the human.
    pub verification_code: String,
}

/// `PairApprovalStatus`: secret-free progress, for the approver only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairApprovalStatusRequest {
    /// [`PAIRING_CEREMONY_VERSION`].
    pub ceremony_version: u32,
    /// The request the approver bound a code to.
    pub ephemeral_id: String,
}

