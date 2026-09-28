//! The approver's side of one pairing request: the code it generates, and the
//! three requests that code becomes.
//!
//! Separate from the requester's session because the two halves are driven by
//! different people on different devices and share only the request id. What
//! they do share is the rule that the code an approval binds is the code the
//! human was already told, which is why it is generated once, persisted, and
//! read back rather than regenerated on a retry — see `ceremony_store`.
//!
//! Ported from `apps/web/src/client/auth/pairing-approval.ts` and the approver
//! half of `onboarding-pairing-ceremony.ts`.

use crate::client::auth::ceremony::{
    PAIRING_CEREMONY_VERSION, RandomSource, generate_pair_verification_code,
    normalize_pair_request_id,
};
use crate::client::auth::pairing_requests::{
    PairApprovalStatusRequest, PairApproveRequest, PairDenyRequest,
};
use crate::client::auth::pairing_session::PairingError;

/// An approver's side of one request: the generated code, and the request it
/// becomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairApproval {
    /// Always [`PAIRING_CEREMONY_VERSION`].
    pub ceremony_version: u32,
    /// The request being approved.
    pub ephemeral_id: String,
    /// The six digits the human reads out and the requester types.
    pub verification_code: String,
    /// What to call this requester in the list.
    pub requester_label: String,
    /// When the request expires, in milliseconds.
    pub expires_at_ms: u64,
}

impl PairApproval {
    /// Approve `ephemeral_id` with a freshly generated code.
    pub fn generate(
        source: &dyn RandomSource,
        ephemeral_id: &str,
        requester_label: &str,
        expires_at_ms: u64,
    ) -> Result<Self, PairingError> {
        let ephemeral_id =
            normalize_pair_request_id(ephemeral_id).ok_or(PairingError::MalformedRequestId)?;
        Ok(Self {
            ceremony_version: PAIRING_CEREMONY_VERSION,
            ephemeral_id,
            verification_code: generate_pair_verification_code(source)?,
            requester_label: requester_label.to_string(),
            expires_at_ms,
        })
    }

    /// The `PairApprove` body.
    ///
    /// Built from the PERSISTED code rather than from a fresh one, so an
    /// approval that was interrupted and retried after a reload binds the same
    /// code the human was already told.
    pub fn approve_request(&self) -> PairApproveRequest {
        PairApproveRequest {
            ephemeral_id: self.ephemeral_id.clone(),
            ceremony_version: self.ceremony_version,
            verification_code: self.verification_code.clone(),
        }
    }

    /// The `PairDeny` body.
    pub fn deny_request(&self) -> PairDenyRequest {
        PairDenyRequest {
            ephemeral_id: self.ephemeral_id.clone(),
        }
    }

    /// The `PairApprovalStatus` body.
    pub fn status_request(&self) -> PairApprovalStatusRequest {
        PairApprovalStatusRequest {
            ceremony_version: self.ceremony_version,
            ephemeral_id: self.ephemeral_id.clone(),
        }
    }
}
