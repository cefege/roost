//! The seven coordinator methods the browser pairing ceremony performs.
//!
//! Called by roost-web's pairing surface: the requester half through
//! `CoordRpc::call_public` (a browser with no device key must still be able to
//! ask), the approver half through `CoordRpc::call`. The split is the
//! coordinator's own table, not a UI decision — `method_route_rows.rs:199-205`
//! gives `PairCreate`, `PairPoll` and `PairConfirm` `AuthRequirement::Public`
//! and the other four `DeviceOrOwnWorkerRecovery`, and `pair_create`,
//! `pair_poll` and `pair_confirm` are the only three pair handlers in
//! `service_impl.rs` that never call `caller_of`.
//!
//! The request bodies are the `client::auth` ones, wrapped rather than
//! restated: a second copy of `PairCreateRequest` here would be a second
//! hand-maintained spelling of one wire message. The answers reuse the auth
//! response types for the same reason, and only `PairApprove`/`PairDeny` — which
//! carry a bare `ok` with no client-side state machine behind them — get a type
//! of their own.
//!
//! v2 call sites: `apps/web/src/store/auth/pair-approval-lifecycle.ts:128-147`
//! (`pairApprovalStatus`, `pairDeny`), `PairApprovalProvider.tsx:183-187`
//! (`pairApprove`), `onboarding-pairing-ceremony.ts:202,247,343`
//! (`pairCreate`, `pairPoll`, `pairConfirm`) and
//! `store/auth/redeemPairToken.ts:36` (`authRedeemBrowser`).

use roost_proto::{
    AuthRedeemBrowserRequest as PbRedeemRequest, AuthRedeemBrowserResponse as PbRedeemResponse,
    PairApprovalStatusRequest as PbStatusRequest, PairApprovalStatusResponse as PbStatusResponse,
    PairApproveRequest as PbApproveRequest, PairApproveResponse as PbApproveResponse,
    PairConfirmRequest as PbConfirmRequest, PairConfirmResponse as PbConfirmResponse,
    PairCreateRequest as PbCreateRequest, PairCreateResponse as PbCreateResponse,
    PairDenyRequest as PbDenyRequest, PairDenyResponse as PbDenyResponse,
    PairPollRequest as PbPollRequest, PairPollResponse as PbPollResponse,
};

use crate::client::auth::pairing_requests::{
    PairApprovalStatusRequest, PairApproveRequest, PairConfirmRequest, PairConfirmResponse,
    PairCreateRequest, PairCreateResponse, PairDenyRequest, PairPollRequest, PairPollResponse,
};
use crate::client::auth::redeem::AuthRedeemBrowserRequest;
use crate::client::rpc::codec::{RpcCodecError, decode_message, encode_message};
use crate::client::rpc::unary::UnaryMethod;

/// `PairCreate`: register a request bound to this browser's public key.
///
/// **Public.** It is the first call an unpaired browser makes, so presenting a
/// credential is not an option it has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatePair {
    /// The body, built by `PairingSession::create_request`.
    pub request: PairCreateRequest,
}

impl UnaryMethod for CreatePair {
    const METHOD: &'static str = "PairCreate";
    type Response = PairCreateResponse;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &PbCreateRequest {
                ssh_pubkey_b64: self.request.ssh_pubkey_b64.clone(),
                label: self.request.label.clone(),
                ceremony_version: self.request.ceremony_version,
                ephemeral_id: self.request.ephemeral_id.clone(),
                requester_token: self.request.requester_token.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<PairCreateResponse, RpcCodecError> {
        let response: PbCreateResponse = decode_message(Self::METHOD, body)?;
        Ok(PairCreateResponse {
            ephemeral_id: response.ephemeral_id,
        })
    }
}

/// `PairPoll`: ask what became of a request this browser owns.
///
/// **Public**, and token-bound: the requester token in the body, not a device
/// key, is what answers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollPair {
    /// The body, built by `PairingSession::poll_request`.
    pub request: PairPollRequest,
}

impl UnaryMethod for PollPair {
    const METHOD: &'static str = "PairPoll";
    type Response = PairPollResponse;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &PbPollRequest {
                ephemeral_id: self.request.ephemeral_id.clone(),
                requester_token: self.request.requester_token.clone(),
                ceremony_version: self.request.ceremony_version,
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<PairPollResponse, RpcCodecError> {
        let response: PbPollResponse = decode_message(Self::METHOD, body)?;
        Ok(PairPollResponse {
            status: response.status,
            expires_at_ms: response.expires_at_ms,
        })
    }
}

/// `PairConfirm`: bind the approver's code to this browser's request.
///
/// **Public**, for the same reason `PairCreate` is: the browser confirming is by
/// definition not yet an authorized device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmPair {
    /// The body, built by `PairingSession::confirm_request`.
    pub request: PairConfirmRequest,
}

impl UnaryMethod for ConfirmPair {
    const METHOD: &'static str = "PairConfirm";
    type Response = PairConfirmResponse;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &PbConfirmRequest {
                ephemeral_id: self.request.ephemeral_id.clone(),
                requester_token: self.request.requester_token.clone(),
                verification_code: self.request.verification_code.clone(),
                ceremony_version: self.request.ceremony_version,
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<PairConfirmResponse, RpcCodecError> {
        let response: PbConfirmResponse = decode_message(Self::METHOD, body)?;
        Ok(PairConfirmResponse { ok: response.ok })
    }
}

/// A pair RPC whose whole answer is whether the coordinator agreed.
///
/// `ok: false` is the coordinator's considered "no" — a wrong code, a request
/// that has already left the live states — and is not a transport failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PairAcknowledged {
    /// Whether the coordinator accepted the call.
    pub ok: bool,
}

/// `PairApprove`: bind a generated code to a pending request.
///
/// **Device-authenticated.** Only an already-trusted browser may approve one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovePair {
    /// The body, built by `PairApproval::approve_request`.
    pub request: PairApproveRequest,
}

impl UnaryMethod for ApprovePair {
    const METHOD: &'static str = "PairApprove";
    type Response = PairAcknowledged;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &PbApproveRequest {
                ephemeral_id: self.request.ephemeral_id.clone(),
                ceremony_version: self.request.ceremony_version,
                verification_code: self.request.verification_code.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<PairAcknowledged, RpcCodecError> {
        let response: PbApproveResponse = decode_message(Self::METHOD, body)?;
        Ok(PairAcknowledged { ok: response.ok })
    }
}

/// `PairDeny`: refuse a request the approver is looking at.
///
/// **Device-authenticated**, and this is what a closed verification dialog
/// issues: an approval nobody will confirm must be withdrawn server-side rather
/// than left for the request's own expiry to reap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenyPair {
    /// The request to refuse.
    pub request: PairDenyRequest,
}

impl UnaryMethod for DenyPair {
    const METHOD: &'static str = "PairDeny";
    type Response = PairAcknowledged;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &PbDenyRequest {
                ephemeral_id: self.request.ephemeral_id.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<PairAcknowledged, RpcCodecError> {
        let response: PbDenyResponse = decode_message(Self::METHOD, body)?;
        Ok(PairAcknowledged { ok: response.ok })
    }
}

/// What `PairApprovalStatus` reports about a request this approver bound a code
/// to.
///
/// `Unknown` is a case rather than an error, for the reason
/// `PairPollStatus::Unknown` is: a coordinator that grows a status must retire
/// this approver's code rather than wedge a client that has never heard of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairApprovalStatus {
    /// The requester has not proved the code yet; keep the code on screen.
    VerificationRequired,
    /// The requester proved it. Terminal, and the happy one.
    Completed,
    /// Someone refused. Terminal.
    Denied,
    /// The request timed out. Terminal.
    Expired,
    /// The attempt limit was reached. Terminal.
    VerificationFailed,
    /// A status this client does not know.
    Unknown(String),
}

impl PairApprovalStatus {
    /// Read a wire status name.
    pub fn from_wire(status: &str) -> Self {
        match status {
            "verification_required" => Self::VerificationRequired,
            "completed" => Self::Completed,
            "denied" => Self::Denied,
            "expired" => Self::Expired,
            "verification_failed" => Self::VerificationFailed,
            other => Self::Unknown(other.to_string()),
        }
    }

    /// The wire spelling, or `None` for a status this client does not know.
    pub fn as_wire(&self) -> Option<&'static str> {
        match self {
            Self::VerificationRequired => Some("verification_required"),
            Self::Completed => Some("completed"),
            Self::Denied => Some("denied"),
            Self::Expired => Some("expired"),
            Self::VerificationFailed => Some("verification_failed"),
            Self::Unknown(_) => None,
        }
    }

    /// Whether this status retires the approver's code.
    pub const fn is_terminal(&self) -> bool {
        !matches!(self, Self::VerificationRequired)
    }
}

/// `PairApprovalStatus`: secret-free progress of one approval.
///
/// **Device-authenticated**, and in `DEVICE_AUTH_REQUIRED_METHODS` because its
/// `Unauthenticated` is a device rejection rather than a retryable one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadPairApprovalStatus {
    /// The body, built by `PairApproval::status_request`.
    pub request: PairApprovalStatusRequest,
}

impl UnaryMethod for ReadPairApprovalStatus {
    const METHOD: &'static str = "PairApprovalStatus";
    type Response = PairApprovalStatus;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &PbStatusRequest {
                ceremony_version: self.request.ceremony_version,
                ephemeral_id: self.request.ephemeral_id.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<PairApprovalStatus, RpcCodecError> {
        let response: PbStatusResponse = decode_message(Self::METHOD, body)?;
        Ok(PairApprovalStatus::from_wire(&response.status))
    }
}

/// `AuthRedeemBrowser`: spend a one-time setup token on this device's key.
///
/// **Public.** A fresh browser has no authorized key to present, and the grant
/// is what authorizes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedeemPairToken {
    /// The body: the grant, this key, and the label the device list will carry.
    pub request: AuthRedeemBrowserRequest,
}

impl UnaryMethod for RedeemPairToken {
    const METHOD: &'static str = "AuthRedeemBrowser";
    type Response = ();

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &PbRedeemRequest {
                token: self.request.token.clone(),
                ssh_pubkey_b64: self.request.ssh_pubkey_b64.clone(),
                label: self.request.label.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<(), RpcCodecError> {
        decode_message::<PbRedeemResponse>(Self::METHOD, body).map(|_| ())
    }
}
