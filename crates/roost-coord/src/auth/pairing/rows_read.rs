//! The two `pair_requests` reads the ceremony makes, and the one column
//! projection they share. Split from `rows.rs`, which owns the row shape and
//! every write that decides a status.
//!
//! ONE PROJECTION FOR TWO SELECTS that differ only in their `WHERE`, because
//! `sqlx` maps positionally and twelve anonymous `Option<String>`s are twelve
//! ways to end up comparing a status against a digest.

use sqlx::Any;

use super::PairingResult;
use super::rows::PairRequestRow;
use super::status::StoredStatus;
/// Read one live request by the ceremony's own handle, or `None`.
///
/// `pending` and `verification_required` are the only statuses a second
/// `PairCreate` has to reconcile against: a terminal row can never be revived,
/// Read one request by its ceremony handle, WHETHER IT IS LIVE OR DECIDED.
///
/// The decided rows matter, and this is the whole reason it is a separate
/// function from [`read_live_pair_request`]. Two callers must be able to tell
/// "no such id" from "this id is already decided":
///
/// - `create_pair_request` answers a re-create of a decided id with
///   `FailedPrecondition: pair request is already terminal`
///   (`pairing-account.ts:66`). Scoped to live rows, the same id reads as
///   absent, the insert runs, and `ephemeral_id`'s UNIQUE index rejects it --
///   so the browser gets a 500 for something that is a normal, expected
///   progression.
/// - `handle_pair_approve` answers a decided id with
///   `FailedPrecondition: pair request is not pending`
///   (`pairing-account.ts:229-231`). Scoped to live rows it would say
///   `NotFound` instead, which tells the operator their approval vanished.
///
/// The idempotent replay of a LIVE request -- the case that motivated the
/// narrower read -- is `CreateOutcome::Retry`, and this function returns it.
pub async fn read_pair_request<'a, E>(
    executor: E,
    ephemeral_id: &str,
) -> PairingResult<Option<PairRequestRow>>
where
    E: sqlx::Executor<'a, Database = Any>,
{
    let row = sqlx::query_as::<_, PairRequestColumns>(
        "SELECT id, ephemeral_id, status, ceremony_version, expires_at_ms, label, \
                requester_token_hash, public_key, approved_account_id, approved_by_fp, \
                verification_code_hash, verification_attempts \
           FROM pair_requests WHERE ephemeral_id = $1",
    )
    .bind(ephemeral_id)
    .fetch_optional(executor)
    .await
    .map_err(|error| super::sqlx_error("pairing.read", error))?;
    Ok(match row {
        Some(columns) => Some(columns.into_row()?),
        None => None,
    })
}

/// Read one request by its ceremony handle, but ONLY if it is still live.
///
/// Exactly one caller wants the narrower question: `PairDeny` scopes its own
/// `WHERE` to the two live states and answers `NotFound` for anything else
/// (`handlers-pairing.ts:329-334`). A decided request is not something an
/// approver denies, it is something they are told is gone.
pub async fn read_live_pair_request<'a, E>(
    executor: E,
    ephemeral_id: &str,
) -> PairingResult<Option<PairRequestRow>>
where
    E: sqlx::Executor<'a, Database = Any>,
{
    let row = sqlx::query_as::<_, PairRequestColumns>(
        "SELECT id, ephemeral_id, status, ceremony_version, expires_at_ms, label, \
                requester_token_hash, public_key, approved_account_id, approved_by_fp, \
                verification_code_hash, verification_attempts \
           FROM pair_requests \
          WHERE ephemeral_id = $1 AND status IN ('pending', 'verification_required')",
    )
    .bind(ephemeral_id)
    .fetch_optional(executor)
    .await
    .map_err(|error| super::sqlx_error("pairing.read_live", error))?;
    Ok(match row {
        Some(columns) => Some(columns.into_row()?),
        None => None,
    })
}

/// The `pair_requests` columns the ceremony reads, in the reads' own order.
///
/// A named projection rather than a bare tuple, because `sqlx` maps
/// positionally and twelve anonymous `Option<String>`s are twelve ways to end
/// up comparing a status against a digest. The conversion is separate from the
/// fetch because two columns are not `Decode`: `status` must be a spelling this
/// coordinator recognises, and `public_key` must never be zero-padded.
#[derive(Debug, sqlx::FromRow)]
struct PairRequestColumns {
    id: String,
    ephemeral_id: String,
    status: String,
    ceremony_version: i64,
    expires_at_ms: i64,
    label: String,
    requester_token_hash: String,
    public_key: Vec<u8>,
    approved_account_id: Option<String>,
    approved_by_fp: Option<String>,
    verification_code_hash: Option<String>,
    verification_attempts: i64,
}

impl PairRequestColumns {
    /// The decoded row, refusing the two column types that cannot be decoded
    /// safely by the database layer.
    fn into_row(self) -> PairingResult<PairRequestRow> {
        // A stored key of the wrong length is a database fault, not a missing
        // row: reading it as absent would turn a repairable migration bug into
        // "this request cannot be confirmed", with nothing to act on.
        let public_key: [u8; 32] = self.public_key.try_into().map_err(|_| {
            super::PairingError::fault(
                "coord.pairing.read: stored pair_requests.public_key is not 32 bytes",
            )
        })?;
        Ok(PairRequestRow {
            id: self.id,
            ephemeral_id: self.ephemeral_id,
            status: StoredStatus::parse(&self.status)?,
            ceremony_version: self.ceremony_version,
            expires_at_ms: self.expires_at_ms,
            label: self.label,
            requester_token_hash: self.requester_token_hash,
            public_key,
            approved_account_id: self.approved_account_id,
            approved_by_fp: self.approved_by_fp,
            verification_code_hash: self.verification_code_hash,
            verification_attempts: self.verification_attempts,
        })
    }
}
