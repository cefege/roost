//! Pairing's durable request creation and its approval transition: the two
//! writes that turn an anonymous browser's ask into a request an operator can
//! decide.
//!
//! Owned by the pairing slice. Ported from
//! `apps/coord/src/auth/pairing-account.ts`; the lifecycle rules it enforces
//! are types in [`super::status`], and this module is the SQL those types
//! select. The account, device and revocation facts an approval depends on are
//! [`super::authority`]'s.
//!
//! EVERY WRITE HERE GOES THROUGH THE STATE MACHINE, NEVER PAST IT. Each
//! function takes a `LiveRequest` or an `ApprovalOutcome` and issues exactly
//! the statement that outcome names, so the `WHERE status = ...` guard on a
//! statement and the rule the caller was handed cannot be two different rules.
//!
//! THE RE-CREATE RULE. `PairCreate` is idempotent on its own id, and the shape
//! of that idempotence is the security property: a second create carrying a
//! different public key, label, requester token or ceremony version is
//! `AlreadyExists`, never a silent overwrite, because an overwrite would let
//! whoever guesses an id replace the key an operator is about to authorize.

use sqlx::{Sqlite, Transaction};

use super::authority;
use super::provenance::{ClientDeviceType, PairRequestProvenance};
use super::rows::{
    LiveSelector, PairRequestRow, count_live, mark_expired, read_pair_request, terminalize,
};
use super::secrets::{MAX_PENDING_PAIR_REQUESTS, PAIRING_CEREMONY_VERSION};
use super::status::{ApprovalAuthority, ApprovalOutcome, LiveRequest, TerminalRequest};
use super::{PairingRefusal, PairingResult, refuse};
use crate::auth::authorized_keys::fingerprint_of_raw_public_key;
use crate::db::CoordDb;

/// What `PairCreate` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateOutcome {
    /// A new `pending` row exists. `expired_ids` are the requests this call
    /// terminalized on its way in, and every one of them owes the bus a
    /// `removed` frame.
    Created { expired_ids: Vec<String> },
    /// This id was already live with exactly this request: the browser's first
    /// `PairCreate` never landed and it is retrying.
    Retry { expired_ids: Vec<String> },
    /// This id was live and has just expired. The caller refuses, and the bus
    /// drops it.
    Expired { expired_ids: Vec<String> },
}

/// Everything one `PairCreate` needs, already validated by the handler.
#[derive(Debug, Clone)]
pub struct PairRequestCreate<'a> {
    /// The ceremony's opaque handle, already normalized.
    pub ephemeral_id: &'a str,
    /// The requester token's digest. Never the token.
    pub requester_token_hash: &'a str,
    /// The requester's raw ed25519 public key.
    pub public_key: [u8; 32],
    /// The operator-facing device label the browser sent.
    pub label: &'a str,
    /// When this call is happening.
    pub now_ms: i64,
    /// When the request stops being redeemable.
    pub expires_at_ms: i64,
    /// What the coordinator observed about the requester.
    pub provenance: &'a PairRequestProvenance,
    /// The front-door provider that vouched for the requester, if any.
    pub edge_identity_provider: Option<&'a str>,
    /// The front-door identity that vouched for the requester, if any.
    pub edge_identity: Option<&'a str>,
}

/// Create the request, or explain the id it already names.
///
/// One transaction, because the ceiling check and the insert have to see the
/// same world: two coordinators both counting 31 live requests and both
/// inserting is a cap that is not a cap.
pub async fn create_pair_request(
    database: &CoordDb,
    input: &PairRequestCreate<'_>,
) -> PairingResult<CreateOutcome> {
    let mut transaction = database
        .pool()
        .begin()
        .await
        .map_err(|error| super::sqlx_error("pairing.create", error))?;
    let outcome = match read_pair_request(&mut *transaction, input.ephemeral_id).await? {
        Some(existing) => reconcile_existing(&mut transaction, existing, input).await?,
        None => insert_fresh(&mut transaction, input).await?,
    };
    transaction
        .commit()
        .await
        .map_err(|error| super::sqlx_error("pairing.create", error))?;
    Ok(outcome)
}

/// A second create of a live id: the idempotent retry, or a refusal.
async fn reconcile_existing(
    transaction: &mut Transaction<'_, Sqlite>,
    existing: PairRequestRow,
    input: &PairRequestCreate<'_>,
) -> PairingResult<CreateOutcome> {
    if !existing.status.is_live() {
        return Err(refuse(PairingRefusal::AlreadyTerminal));
    }
    // The row is live, so this is a retry only if it is the SAME request.
    // Anything else -- a different ceremony, label, token or key -- is a
    // takeover attempt on an id somebody else already holds, and it is refused
    // rather than merged.
    if !existing.speaks_current_ceremony()
        || existing.label != input.label
        || existing.requester_token_hash != input.requester_token_hash
        || existing.public_key != input.public_key
    {
        return Err(refuse(PairingRefusal::IdAlreadyExists));
    }
    if existing.expires_at_ms > input.now_ms {
        return Ok(CreateOutcome::Retry {
            expired_ids: Vec::new(),
        });
    }
    let expired = mark_expired(&mut *transaction, &existing, input.now_ms).await?;
    Ok(CreateOutcome::Expired { expired_ids: expired })
}

/// A first create: refuse a revoked key, reclaim the dead, enforce the cap, and
/// insert.
async fn insert_fresh(
    transaction: &mut Transaction<'_, Sqlite>,
    input: &PairRequestCreate<'_>,
) -> PairingResult<CreateOutcome> {
    let requester_fingerprint = fingerprint_of_raw_public_key(&input.public_key);
    if authority::is_revoked(&mut **transaction, &requester_fingerprint).await? {
        return Err(refuse(PairingRefusal::KeyRevoked));
    }
    let expired = terminalize(
        &mut **transaction,
        LiveSelector::ExpiredBy(input.now_ms),
        TerminalRequest::Expired,
        input.now_ms,
    )
    .await?;
    let replaced = terminalize(
        &mut **transaction,
        LiveSelector::SameKey(input.public_key),
        TerminalRequest::Expired,
        input.now_ms,
    )
    .await?;
    let live = count_live(&mut **transaction).await?;
    if live >= MAX_PENDING_PAIR_REQUESTS {
        tracing::warn!(
            live,
            client_ip = %input.provenance.source_ip(),
            "refusing a pair request at the live-request ceiling"
        );
        return Err(refuse(PairingRefusal::TooManyLiveRequests));
    }
    insert_request(&mut **transaction, input, &requester_fingerprint).await?;
    Ok(CreateOutcome::Created {
        expired_ids: [expired, replaced].concat(),
    })
}

/// Apply the approval the state machine decided on, and report what it did.
pub async fn apply_approval(
    database: &CoordDb,
    live: LiveRequest,
    approval: &ApprovalAuthority,
    verification_code_hash: &str,
    now_ms: i64,
) -> PairingResult<ApprovalOutcome> {
    let identity = live.identity().clone();
    match live.approve(approval, verification_code_hash, now_ms) {
        ApprovalOutcome::Retry => Ok(ApprovalOutcome::Retry),
        ApprovalOutcome::Refused(refusal) => Err(refuse(refusal)),
        ApprovalOutcome::Expired => {
            terminalize(
                database.pool(),
                LiveSelector::ById(identity.id),
                TerminalRequest::Expired,
                now_ms,
            )
            .await?;
            Ok(ApprovalOutcome::Expired)
        }
        ApprovalOutcome::Approved { identity } => {
            let changed = sqlx::query(
                "UPDATE pair_requests \
                    SET status = 'verification_required', ceremony_version = ?, \
                        verification_code_hash = ?, verification_attempts = 0, \
                        approved_by_fp = ?, approved_account_id = ? \
                  WHERE id = ? AND status = 'pending' AND expires_at_ms > ? \
                    AND ceremony_version = ?",
            )
            .bind(i64::from(PAIRING_CEREMONY_VERSION))
            .bind(verification_code_hash)
            .bind(&approval.approver_fingerprint)
            .bind(&approval.account_id)
            .bind(identity.id)
            .bind(now_ms)
            .bind(i64::from(PAIRING_CEREMONY_VERSION))
            .execute(database.pool())
            .await
            .map_err(|error| super::sqlx_error("pairing.approve", error))?
            .rows_affected();
            if changed != 1 {
                // The guard lost a race with the retention sweep or a
                // concurrent deny. The row is no longer `pending`, so the
                // approval did not happen, and saying so is the only honest
                // answer.
                return Err(refuse(PairingRefusal::NotPending));
            }
            Ok(ApprovalOutcome::Approved { identity })
        }
    }
}

/// Apply the approval the state machine decided on, and report what it did.
pub async fn apply_approval(
    database: &CoordDb,
    live: LiveRequest,
    approval: &ApprovalAuthority,
    verification_code_hash: &str,
    now_ms: i64,
) -> PairingResult<ApprovalOutcome> {
    let identity = live.identity().clone();
    match live.approve(approval, verification_code_hash, now_ms) {
        ApprovalOutcome::Retry => Ok(ApprovalOutcome::Retry),
        ApprovalOutcome::Refused(refusal) => Err(refuse(refusal)),
        ApprovalOutcome::Expired => {
            terminalize(
                database.pool(),
                LiveSelector::ById(identity.id),
                TerminalRequest::Expired,
                now_ms,
            )
            .await?;
            Ok(ApprovalOutcome::Expired)
        }
        ApprovalOutcome::Approved { identity } => {
            let changed = sqlx::query(
                "UPDATE pair_requests \
                    SET status = 'verification_required', ceremony_version = ?, \
                        verification_code_hash = ?, verification_attempts = 0, \
                        approved_by_fp = ?, approved_account_id = ? \
                  WHERE id = ? AND status = 'pending' AND expires_at_ms > ? \
                    AND ceremony_version = ?",
            )
            .bind(i64::from(PAIRING_CEREMONY_VERSION))
            .bind(verification_code_hash)
            .bind(&approval.approver_fingerprint)
            .bind(&approval.account_id)
            .bind(identity.id)
            .bind(now_ms)
            .bind(i64::from(PAIRING_CEREMONY_VERSION))
            .execute(database.pool())
            .await
            .map_err(|error| super::sqlx_error("pairing.approve", error))?
            .rows_affected();
            if changed != 1 {
                // The guard lost a race with the retention sweep or a
                // concurrent deny. The row is no longer `pending`, so the
                // approval did not happen, and saying so is the only honest
                // answer.
                return Err(refuse(PairingRefusal::NotPending));
            }
            Ok(ApprovalOutcome::Approved { identity })
        }
    }
}
