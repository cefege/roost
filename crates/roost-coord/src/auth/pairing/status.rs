//! The pair-request lifecycle, as a type rather than a chain of conditionals.
//!
//! Owned by the pairing slice. `protocol/spec/auth-and-pairing.md` §State
//! machine is the definition; `apps/coord/src/auth/pairing-approval-status.ts`
//! is v2's implementation of the read half, and `pairing-account.ts` /
//! `pairing-confirmation.ts` the write halves.
//!
//! WHY A TYPESTATE AND NOT AN `if`. v2 decides every transition with a
//! `status === "..."` comparison inside the handler that performs it
//! (`pairing-account.ts:205-238`, `pairing-confirmation.ts:117-199`), so the
//! rule "only a request awaiting verification may be confirmed" exists in four
//! places, in four spellings, and nothing stops a fifth from writing
//! `WHERE status = 'pending'` on the confirm path. Here a transition is a
//! method on the value it starts from: `LiveRequest::approve` and
//! `LiveRequest::deny` exist on the live phases, and
//! `LiveRequest::into_approved` -- the only door to the type that has
//! `ApprovedRequest::complete` and `ApprovedRequest::fail_verification` --
//! refuses a `pending` row. A confirm transition applied to a request nobody
//! approved is not merely untested; it does not compile.
//!
//! EXPIRY IS A QUESTION, NOT A STATE YOU MOVE INTO. Both live phases carry a
//! deadline and every caller asks `LiveRequest::is_expired_at` before deciding
//! anything, because v2's expiry handling is a read-only normalization in
//! three separate places (`pairing-approval-status.ts:43`,
//! `handlers-pairing.ts:166`, `pairing-account.ts:205`) and a fourth that had
//! to repeat it is how a requester gets told `expired` by Poll and
//! `verification_required` by Approve.

use roost_protocol::ProtocolResult;

use super::PairingRefusal;
use super::secrets::PAIR_VERIFICATION_ATTEMPT_LIMIT;

/// A `pair_requests.status` this coordinator recognises.
///
/// `parse` is the only way a stored string becomes one of these, so an
/// unrecognised status is a database fault naming itself rather than a value
/// that falls through every comparison as "not live" -- the failure mode a
/// string-typed status has in every language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoredStatus {
    /// Live: an approver has not yet bound a code.
    Pending,
    /// Live: an approver bound a code; the requester may confirm.
    VerificationRequired,
    /// Terminal: an approver refused.
    Denied,
    /// Terminal: the ten-minute lifetime ran out.
    Expired,
    /// Terminal: the confirmation attempt series ended without the right code.
    VerificationFailed,
    /// Terminal: the requester confirmed and the device is authorized.
    Completed,
}

impl StoredStatus {
    /// The value `pair_requests.status` stores.
    #[must_use]
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::VerificationRequired => "verification_required",
            Self::Denied => "denied",
            Self::Expired => "expired",
            Self::VerificationFailed => "verification_failed",
            Self::Completed => "completed",
        }
    }

    /// Whether the request can still be decided.
    #[must_use]
    pub fn is_live(self) -> bool {
        matches!(self, Self::Pending | Self::VerificationRequired)
    }

    /// Decode a stored status, refusing a value this coordinator never writes.
    pub fn parse(raw: &str) -> ProtocolResult<Self> {
        match raw {
            "pending" => Ok(Self::Pending),
            "verification_required" => Ok(Self::VerificationRequired),
            "denied" => Ok(Self::Denied),
            "expired" => Ok(Self::Expired),
            "verification_failed" => Ok(Self::VerificationFailed),
            "completed" => Ok(Self::Completed),
            other => Err(roost_protocol::ProtocolError::new(
                "coord.pairing.status",
                format!("unrecognised pair request status {other:?}"),
            )),
        }
    }
}

/// A decided pair request. Every arm is a durable row that will never change
/// again, which is why these values carry no identity and no deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalRequest {
    /// An approver refused it.
    Denied,
    /// The lifetime ran out before anybody decided it.
    Expired,
    /// The confirmation attempt series ended without the bound code.
    VerificationFailed,
    /// The requester confirmed; its key is now an authorized key.
    Completed,
}

impl TerminalRequest {
    /// The value `pair_requests.status` stores.
    #[must_use]
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Denied => StoredStatus::Denied.as_wire(),
            Self::Expired => StoredStatus::Expired.as_wire(),
            Self::VerificationFailed => StoredStatus::VerificationFailed.as_wire(),
            Self::Completed => StoredStatus::Completed.as_wire(),
        }
    }
}

/// The row identity every transition needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestIdentity {
    /// `pair_requests.id`, the surrogate key the transitions write through.
    pub id: i64,
    /// The ceremony's opaque handle; the value the bus and every log line use.
    pub ephemeral_id: String,
    /// When the request stops being redeemable.
    pub expires_at_ms: i64,
}

/// A live pair request: nobody has denied it, and it has not expired.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveRequest {
    /// `pending`. Nobody has bound a verification code.
    AwaitingApproval(RequestIdentity),
    /// `verification_required`. An approver bound a code and an account, and
    /// only the requester holding the requester token can complete it.
    AwaitingConfirmation(ApprovedRequest),
}

/// A live request an approver has already acted on.
///
/// Separate from [`LiveRequest`] so the confirmation-only transitions cannot
/// be reached from `pending`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovedRequest {
    /// Which row this is, and when it stops being redeemable.
    pub identity: RequestIdentity,
    /// The account the approval was granted under, or `None` for a direct
    /// on-host approval with no account device behind it.
    pub approved_account_id: Option<String>,
    /// The approving key's fingerprint, or `None` for a direct on-host
    /// approval. `PairApprovalStatus` admits the caller on exactly this.
    pub approved_by_fingerprint: Option<String>,
    /// The bound code's digest, never a code.
    ///
    /// A digest because that is what `pair_requests` stores, and a second
    /// approval has to be compared against it to be idempotent. It is the
    /// SHA-256 of a six-digit value, so it is not a credential: it is only ever
    /// compared for equality, never logged and never returned.
    pub verification_code_hash: Option<String>,
    /// Wrong codes already presented against this request.
    pub verification_attempts: i64,
}

/// Who is approving, and under which account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalAuthority {
    /// The account the new device will belong to.
    pub account_id: String,
    /// The approving key, or `None` for a direct on-host approval.
    pub approver_fingerprint: Option<String>,
}

/// What an approval did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalOutcome {
    /// The code was bound: the row is now `verification_required`, and the
    /// caller owes exactly one `UPDATE ... WHERE status = 'pending'`.
    Approved {
        /// The row that moved.
        identity: RequestIdentity,
    },
    /// The same approver re-presented the same code. Idempotent, and the
    /// reason a browser whose response was lost can press the button twice
    /// without the second press becoming a second approval.
    Retry,
    /// The lifetime had already run out. The caller owes the durable
    /// `expired` write this outcome names.
    Expired,
    /// Refused, and the caller owes no write at all.
    Refused(PairingRefusal),
}

impl LiveRequest {
    /// Bind a verification code: the only `pending -> verification_required`
    /// transition, and the one that names an already-bound request.
    ///
    /// Takes `self` so a request cannot be decided twice in one scope, and
    /// returns the outcome rather than performing a partial write, so the SQL
    /// in `account.rs` runs only for `Approved` and `Expired`.
    pub fn approve(
        self,
        authority: &ApprovalAuthority,
        verification_code_hash: &str,
        now_ms: i64,
    ) -> ApprovalOutcome {
        match self {
            LiveRequest::AwaitingApproval(identity) => {
                if identity.expires_at_ms <= now_ms {
                    return ApprovalOutcome::Expired;
                }
                ApprovalOutcome::Approved { identity }
            }
            // The row already carries a code. A byte-identical re-presentation
            // by the same approver under the same account is the same
            // approval; anything else is somebody else's code being guessed at,
            // or a second approver trying to take the request over.
            LiveRequest::AwaitingConfirmation(approved) => {
                let same_approval = approved.verification_code_hash.as_deref()
                    == Some(verification_code_hash)
                    && approved.approved_by_fingerprint == authority.approver_fingerprint
                    && approved.approved_account_id.as_deref()
                        == Some(authority.account_id.as_str());
                if same_approval {
                    ApprovalOutcome::Retry
                } else {
                    ApprovalOutcome::Refused(PairingRefusal::AlreadyApproved)
                }
            }
        }
    }

    /// Deny it. Legal from either live phase, because a request an attacker
    /// holds the code for must be revocable after approval.
    #[must_use]
    pub fn deny(self) -> TerminalRequest {
        TerminalRequest::Denied
    }

    /// Expire it. The caller owes the durable write.
    #[must_use]
    pub fn expire(self) -> TerminalRequest {
        TerminalRequest::Expired
    }

    /// Whether the request's lifetime has run out.
    #[must_use]
    pub fn is_expired_at(&self, now_ms: i64) -> bool {
        self.identity().expires_at_ms <= now_ms
    }

    /// Which row this is.
    #[must_use]
    pub fn identity(&self) -> &RequestIdentity {
        match self {
            Self::AwaitingApproval(identity) => identity,
            Self::AwaitingConfirmation(approved) => &approved.identity,
        }
    }

    /// The door to the confirmation-only transitions.
    ///
    /// The one place a live request is checked for a bound code, and the
    /// reason a `pending` row can never reach `ApprovedRequest::complete`.
    pub fn into_approved(self) -> Result<ApprovedRequest, PairingRefusal> {
        match self {
            Self::AwaitingConfirmation(approved) => Ok(approved),
            Self::AwaitingApproval(_) => Err(PairingRefusal::NotAwaitingVerification),
        }
    }
}

impl ApprovedRequest {
    /// A wrong or exhausted code: the terminal
    /// `verification_required -> verification_failed` write.
    #[must_use]
    pub fn fail_verification(self) -> TerminalRequest {
        TerminalRequest::VerificationFailed
    }

    /// The right code: the terminal `verification_required -> completed`
    /// write, and the only one that authorizes a key.
    #[must_use]
    pub fn complete(self) -> TerminalRequest {
        TerminalRequest::Completed
    }

    /// The attempt footprint a wrong code leaves behind.
    ///
    /// Saturates at the limit rather than counting past it, because the
    /// durable column is what the next attempt reads, and a value above the
    /// limit would report a series longer than the one that ended it.
    #[must_use]
    pub fn next_attempt(&self) -> AttemptRecord {
        let attempts = (self.verification_attempts + 1).min(PAIR_VERIFICATION_ATTEMPT_LIMIT);
        AttemptRecord {
            attempts,
            exhausted: attempts >= PAIR_VERIFICATION_ATTEMPT_LIMIT,
        }
    }

    /// Whether this request is a retry of an approval already bound.
    ///
    /// The comparison a second `PairApprove` is judged by, kept here so the
    /// three facts that make an approval the same approval are decided in one
    /// place: the code, the approver, and the account.
    #[must_use]
    pub fn matches_approval(
        &self,
        authority: &ApprovalAuthority,
        verification_code_hash: &str,
    ) -> bool {
        self.verification_code_hash.as_deref() == Some(verification_code_hash)
            && self.approved_by_fingerprint == authority.approver_fingerprint
            && self.approved_account_id.as_deref() == Some(authority.account_id.as_str())
    }

    /// Which row this is.
    #[must_use]
    pub fn identity(&self) -> &RequestIdentity {
        &self.identity
    }
}

/// A wrong code's durable footprint: the new count, and whether it ended the
/// series.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttemptRecord {
    /// The count to store, saturated at the limit.
    pub attempts: i64,
    /// Whether this attempt was the last one the request will accept.
    pub exhausted: bool,
}

/// The three columns the approver's status read needs.
///
/// Separate from the write paths' row because a status read must not be able
/// to reach a public key, a requester-token digest, or a code digest: this
/// struct cannot hold one, so the read cannot leak one even by accident.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalStatusFacts {
    /// The row's status.
    pub status: StoredStatus,
    /// The approving key, or `None` for a direct on-host approval.
    pub approved_by_fingerprint: Option<String>,
    /// When the request stops being redeemable.
    pub expires_at_ms: i64,
}

/// The secret-free progress an approver may read back.
///
/// `Err` is the only other answer, and it is deliberately uniform: a request
/// that does not exist and a request somebody else approved both refuse the
/// same way, so the read cannot be used to discover that an id exists
/// (`pairing-approval-status.ts:33-38,52`).
pub fn read_approval_status(
    facts: Option<&ApprovalStatusFacts>,
    caller_fingerprint: Option<&str>,
    on_host: bool,
    now_ms: i64,
) -> Result<&'static str, PairingRefusal> {
    let Some(facts) = facts else {
        return Err(PairingRefusal::NotFound);
    };
    let admitted = match &facts.approved_by_fingerprint {
        None => on_host,
        Some(approved_by) => caller_fingerprint == Some(approved_by.as_str()),
    };
    if !admitted {
        return Err(PairingRefusal::NotFound);
    }
    Ok(match facts.status {
        // Read-only normalization: this read never writes. The durable expiry
        // is owed by the confirm path and the retention sweep.
        StoredStatus::Pending | StoredStatus::VerificationRequired => {
            if facts.expires_at_ms <= now_ms {
                TerminalRequest::Expired.as_wire()
            } else {
                facts.status.as_wire()
            }
        }
        terminal => terminal.as_wire(),
    })
}
