//! The pairing ceremony's requests and the state machine that orders them.
//!
//! A requester and an approver meet through three values — a request id, a
//! requester token, and a six-digit code — and through a coordinator that grants
//! authority only after a token-bound confirmation. This file is the client half
//! of that: the request shapes, the stages a request moves through, and the rule
//! for each transition.
//!
//! **It owns no timer.** Polling is the host's job, because the host already
//! owns every other interval in this client and a second scheduler here would be
//! a second place a poll cadence lives. What this file gives the host is the
//! stage — `Created`, `Acknowledged`, `VerificationRequired`, `Completed`,
//! `Terminal` — and the requests each stage permits, so a poll that arrives
//! after a denial, or a confirm before the code was ever bound, is refused by
//! name instead of being sent and answered.
//!
//! The one transition worth naming: a `PairCreate` whose echoed `ephemeral_id`
//! is not the one this browser generated is a refusal, not a success. The
//! coordinator answers generically for a request it cannot see, so a matching id
//! is the only evidence the request actually exists.
//!
//! Ported from `apps/web/src/components/pairing/onboarding-pairing-ceremony.ts`
//! and `apps/web/src/client/auth/pairing-approval.ts`; the contract is
//! `protocol/spec/auth-and-pairing.md:26-29`.

use std::fmt;

use crate::client::auth::ceremony::{
    CeremonyError, PAIRING_CEREMONY_VERSION, RandomSource, compact_pair_verification_code,
    generate_pair_request_id, generate_pair_requester_token,
    normalize_pair_request_id, normalize_pair_requester_token,
    normalize_pair_verification_code,
};

use crate::client::auth::pairing_requests::{
    PairConfirmRequest, PairConfirmResponse, PairCreateRequest, PairCreateResponse,
    PairPollRequest, PairPollResponse,
};

/// The identity a requester holds for the length of one ceremony.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingCeremony {
    /// Always [`PAIRING_CEREMONY_VERSION`] for this client.
    pub ceremony_version: u32,
    /// 32 lowercase hex characters; the request's identity.
    pub ephemeral_id: String,
    /// 64 lowercase hex characters; the only thing that can poll or confirm it.
    ///
    /// Tab-scoped and never in rootStore, Sync, a URL or cross-tab storage. It is
    /// the requester's capability, and a ceremony capability that outlived the
    /// tab that created it would let a second tab finish somebody else's pairing.
    pub requester_token: String,
}

/// The six status names `PairPoll` answers with, plus the states only this
/// client is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairPollStatus {
    /// No request has been created.
    Idle,
    /// The coordinator has the request; no approver has bound a code.
    Pending,
    /// An approver bound a code; the requester must enter it.
    VerificationRequired,
    /// The key is enrolled. Terminal, and the happy one.
    Completed,
    /// An approver refused. Terminal.
    Denied,
    /// The request timed out. Terminal.
    Expired,
    /// The attempt limit was reached. Terminal, and the request must be made
    /// again from scratch — the ceremony is single-use by construction.
    VerificationFailed,
    /// A status this client does not know.
    ///
    /// Its own case rather than an error, because a coordinator that grows a
    /// status must not wedge a client that has never heard of it: the ceremony
    /// stops rather than continuing to poll a state it cannot interpret.
    Unknown(String),
}

impl PairPollStatus {
    /// Read a wire status name.
    pub fn from_wire(status: &str) -> Self {
        match status {
            "pending" => Self::Pending,
            "verification_required" => Self::VerificationRequired,
            "completed" => Self::Completed,
            "denied" => Self::Denied,
            "expired" => Self::Expired,
            "verification_failed" => Self::VerificationFailed,
            other => Self::Unknown(other.to_string()),
        }
    }

    /// The wire spelling, or `None` for the two client-only states.
    pub fn as_wire(&self) -> Option<&'static str> {
        match self {
            Self::Pending => Some("pending"),
            Self::VerificationRequired => Some("verification_required"),
            Self::Completed => Some("completed"),
            Self::Denied => Some("denied"),
            Self::Expired => Some("expired"),
            Self::VerificationFailed => Some("verification_failed"),
            Self::Idle | Self::Unknown(_) => None,
        }
    }

    /// Whether this state ends the ceremony.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Denied | Self::Expired | Self::VerificationFailed
        )
    }
}

/// A ceremony transition this client refused to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairingError {
    /// The ceremony's own values could not be produced or read.
    Ceremony(CeremonyError),
    /// The coordinator echoed a different request id than this browser created.
    RequestMismatch,
    /// The request id is not 32 lowercase hex characters.
    ///
    /// Its own case rather than folded into the code's, because the two arrive
    /// from different people: an approver supplied the id, and reporting "the
    /// verification code is malformed" about a bad id sends them to look at the
    /// wrong field.
    MalformedRequestId,
    /// The code is not six digits once its whitespace is removed.
    MalformedVerificationCode,
    /// The request is not valid in the stage this ceremony is in.
    WrongStage {
        /// The stage the ceremony is actually in.
        stage: &'static str,
    },
}

impl fmt::Display for PairingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ceremony(error) => write!(formatter, "{error}"),
            Self::RequestMismatch => write!(
                formatter,
                "the pairing request did not match this browser"
            ),
            Self::MalformedVerificationCode => write!(
                formatter,
                "the verification code is not six digits"
            ),
            Self::MalformedRequestId => write!(
                formatter,
                "the pair request id is not 32 lowercase hex characters"
            ),
            Self::WrongStage { stage } => {
                write!(formatter, "the ceremony is in stage {stage}, not this one")
            }
        }
    }
}

impl std::error::Error for PairingError {}

impl From<CeremonyError> for PairingError {
    fn from(error: CeremonyError) -> Self {
        Self::Ceremony(error)
    }
}

/// Where a ceremony is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairStage {
    /// Values generated, no request sent.
    Created,
    /// The coordinator has the request.
    Acknowledged,
    /// A code is bound and the requester must enter it.
    VerificationRequired,
    /// The key is enrolled.
    Completed,
    /// The ceremony ended without enrolling: denied, expired, or failed.
    Terminal,
}

/// A requester's ceremony, from generated values to a decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingSession {
    ceremony: PairingCeremony,
    stage: PairStage,
    expires_at_ms: u64,
}

impl PairingSession {
    /// Mint a ceremony's values from `source`.
    pub fn create(source: &dyn RandomSource) -> Result<Self, PairingError> {
        Ok(Self {
            ceremony: PairingCeremony {
                ceremony_version: PAIRING_CEREMONY_VERSION,
                ephemeral_id: generate_pair_request_id(source)?,
                requester_token: generate_pair_requester_token(source)?,
            },
            stage: PairStage::Created,
            expires_at_ms: 0,
        })
    }

    /// Rebuild a ceremony from values that were persisted, validating them.
    ///
    /// A stored record that does not validate is `None`, not a partially-trusted
    /// one: the requester token is the only thing that can finish this pairing,
    /// and a half-valid token would produce a poll that can only ever be refused.
    pub fn restore(ceremony: PairingCeremony) -> Option<Self> {
        if ceremony.ceremony_version != PAIRING_CEREMONY_VERSION
            || normalize_pair_request_id(&ceremony.ephemeral_id).is_none()
            || normalize_pair_requester_token(&ceremony.requester_token).is_none()
        {
            return None;
        }
        Some(Self {
            ceremony,
            stage: PairStage::Created,
            expires_at_ms: 0,
        })
    }

    /// The ceremony's values, for persisting and for building requests.
    pub fn ceremony(&self) -> &PairingCeremony {
        &self.ceremony
    }

    /// Where the ceremony is.
    pub fn stage(&self) -> PairStage {
        self.stage
    }

    /// When the request expires, once a poll has said.
    pub fn expires_at_ms(&self) -> u64 {
        self.expires_at_ms
    }

    /// The `PairCreate` body for this browser's public key.
    pub fn create_request(&self, ssh_pubkey_b64: &str, label: &str) -> PairCreateRequest {
        PairCreateRequest {
            ssh_pubkey_b64: ssh_pubkey_b64.to_string(),
            label: label.to_string(),
            ceremony_version: self.ceremony.ceremony_version,
            ephemeral_id: self.ceremony.ephemeral_id.clone(),
            requester_token: self.ceremony.requester_token.clone(),
        }
    }

    /// Record that the coordinator created exactly this request.
    pub fn on_create_response(
        &mut self,
        response: &PairCreateResponse,
    ) -> Result<(), PairingError> {
        if self.stage != PairStage::Created {
            return Err(PairingError::WrongStage {
                stage: self.stage_name(),
            });
        }
        if response.ephemeral_id != self.ceremony.ephemeral_id {
            return Err(PairingError::RequestMismatch);
        }
        self.stage = PairStage::Acknowledged;
        tracing::info!(target: "auth", "auth.pair_created");
        Ok(())
    }

    /// The `PairPoll` body, or `None` before the coordinator has the request.
    pub fn poll_request(&self) -> Option<PairPollRequest> {
        if !matches!(self.stage, PairStage::Acknowledged | PairStage::VerificationRequired) {
            return None;
        }
        Some(PairPollRequest {
            ephemeral_id: self.ceremony.ephemeral_id.clone(),
            requester_token: self.ceremony.requester_token.clone(),
            ceremony_version: self.ceremony.ceremony_version,
        })
    }

    /// Apply a poll answer, returning the status it named.
    pub fn on_poll_response(&mut self, response: &PairPollResponse) -> PairPollStatus {
        let status = PairPollStatus::from_wire(&response.status);
        self.expires_at_ms = response.expires_at_ms;
        self.stage = match &status {
            PairPollStatus::Pending | PairPollStatus::Unknown(_) => self.stage,
            PairPollStatus::VerificationRequired => PairStage::VerificationRequired,
            PairPollStatus::Completed => PairStage::Completed,
            PairPollStatus::Denied | PairPollStatus::Expired | PairPollStatus::VerificationFailed => {
                PairStage::Terminal
            }
            PairPollStatus::Idle => self.stage,
        };
        if status.is_terminal() {
            tracing::info!(
                target: "auth",
                status = status.as_wire().unwrap_or("unknown"),
                "auth.pair_terminal"
            );
        }
        status
    }

    /// The `PairConfirm` body for what the human typed.
    ///
    /// The code is compacted and validated here, so a wrong code is refused
    /// before it reaches the coordinator and before the attempt counter moves.
    pub fn confirm_request(&self, typed: &str) -> Result<PairConfirmRequest, PairingError> {
        if self.stage != PairStage::VerificationRequired {
            return Err(PairingError::WrongStage {
                stage: self.stage_name(),
            });
        }
        let verification_code =
            normalize_pair_verification_code(&compact_pair_verification_code(typed))
                .ok_or(PairingError::MalformedVerificationCode)?;
        Ok(PairConfirmRequest {
            ephemeral_id: self.ceremony.ephemeral_id.clone(),
            requester_token: self.ceremony.requester_token.clone(),
            verification_code,
            ceremony_version: self.ceremony.ceremony_version,
        })
    }

    /// Record a confirmation answer. `false` is a wrong code, not a failure.
    pub fn on_confirm_response(&mut self, response: &PairConfirmResponse) {
        if response.ok {
            self.stage = PairStage::Completed;
            tracing::info!(target: "auth", "auth.pair_confirmed");
        }
    }

    /// The ceremony's stage as a name, for an error that has to carry one.
    fn stage_name(&self) -> &'static str {
        match self.stage {
            PairStage::Created => "created",
            PairStage::Acknowledged => "acknowledged",
            PairStage::VerificationRequired => "verification_required",
            PairStage::Completed => "completed",
            PairStage::Terminal => "terminal",
        }
    }
}
