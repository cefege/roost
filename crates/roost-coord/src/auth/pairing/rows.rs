//! The `pair_requests` row as the ceremony reads it, and the one statement
//! every terminalization runs.
//!
//! Owned by the pairing slice. Split out of `account` because a row shape, a
//! SELECT and an UPDATE that decides every status in the domain belong
//! together: the riskiest pairing bug is a projection whose columns do not
//! match the transition that writes them.
//!
//! THE SELECTION IS A CLOSED SET. Every [`LiveSelector`] variant contributes a
//! `&'static str` it chose, which is what lets [`terminalize`] assemble a
//! statement no request can influence.

use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Row, Sqlite};

use super::PairingResult;
use super::PairingRefusal;
use super::account::PairRequestCreate;
use super::provenance::ClientDeviceType;
use super::refuse;
use super::secrets::{MAX_PENDING_PAIR_REQUESTS, PAIRING_CEREMONY_VERSION};
use super::status::{ApprovedRequest, LiveRequest, RequestIdentity, StoredStatus, TerminalRequest};

/// A live request as `PairCreate` or `PairApprove` found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairRequestRow {
    /// `pair_requests.id`, the surrogate key the transitions write through.
    pub id: i64,
    /// The ceremony's opaque handle.
    pub ephemeral_id: String,
    /// The stored status, decoded.
    pub status: StoredStatus,
    /// The ceremony version the row was created under.
    pub ceremony_version: i64,
    /// When the request stops being redeemable.
    pub expires_at_ms: i64,
    /// The operator-facing device label.
    pub label: String,
    /// The requester token's digest. Never the token.
    pub requester_token_hash: String,
    /// The requester's raw ed25519 public key.
    pub public_key: [u8; 32],
    /// The account the approval was granted under, if any.
    pub approved_account_id: Option<String>,
    /// The approving key, if any.
    pub approved_by_fp: Option<String>,
    /// The bound code's digest, if a code is bound.
    pub verification_code_hash: Option<String>,
    /// Wrong codes already presented.
    pub verification_attempts: i64,
}

impl PairRequestRow {
    /// The identity the transitions write through.
    #[must_use]
    pub fn identity(&self) -> RequestIdentity {
        RequestIdentity {
            id: self.id,
            ephemeral_id: self.ephemeral_id.clone(),
            expires_at_ms: self.expires_at_ms,
        }
    }

    /// The live value this row is, or `None` because it is decided.
    ///
    /// A decided row is `None` rather than an error, because "it is already
    /// terminal" is a stage fact the caller turns into a refusal, and the two
    /// refusals differ: a re-create is `AlreadyTerminal` and a re-approve is
    /// `NotPending`.
    #[must_use]
    pub fn live(&self) -> Option<LiveRequest> {
        match self.status {
            StoredStatus::Pending => Some(LiveRequest::AwaitingApproval(self.identity())),
            StoredStatus::VerificationRequired => {
                Some(LiveRequest::AwaitingConfirmation(ApprovedRequest {
                    identity: self.identity(),
                    approved_account_id: self.approved_account_id.clone(),
                    approved_by_fingerprint: self.approved_by_fp.clone(),
                    verification_code_hash: self.verification_code_hash.clone(),
                    verification_attempts: self.verification_attempts,
                }))
            }
            _ => None,
        }
    }

    /// Whether this row was created under the ceremony this coordinator speaks.
    #[must_use]
    pub fn speaks_current_ceremony(&self) -> bool {
        self.ceremony_version == i64::from(PAIRING_CEREMONY_VERSION)
    }
}

/// What `PairCreate` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveSelector {
    /// One row by surrogate key.
    ById(i64),
    /// Every live row whose deadline has passed at or before the bound.
    ExpiredBy(i64),
    /// Every live row for one public key.
    SameKey([u8; 32]),
    /// At most `limit` live rows whose deadline has passed, oldest first.
    ///
    /// The retention sweep's form. The bound rides on a subselect because
    /// SQLite's `UPDATE ... LIMIT` needs a compile option this build does not
    /// have, and a sweep that cannot bound its own statement is a sweep that
    /// can hold the write lock for its whole backlog.
    ExpiredByBatch { bound: i64, limit: i64 },
}

/// Move the selected live rows to a terminal state, clearing any code digest.
///
/// One statement for every selector: "expire this row", "expire everything
/// overdue" and "expire every request for this key" differ only in which rows
/// they name, and three hand-written copies of an UPDATE that clears
/// `verification_code_hash` are three chances to forget the clear.
pub async fn terminalize<'a, E>(
    executor: E,
    selector: LiveSelector,
    terminal: TerminalRequest,
    now_ms: i64,
) -> PairingResult<Vec<String>>
where
    E: sqlx::Executor<'a, Database = Sqlite>,
{
    let mut statement = QueryBuilder::<Sqlite>::new("UPDATE pair_requests SET status = ");
    statement.push_bind(terminal.as_wire());
    statement.push(", decided_at_ms = ");
    statement.push_bind(now_ms);
    statement.push(", verification_code_hash = NULL");
    push_selection(&mut statement, selector);
    statement.push(" AND status IN ('pending', 'verification_required') RETURNING ephemeral_id");
    let rows: Vec<SqliteRow> = statement
        .build()
        .fetch_all(executor)
        .await
        .map_err(|error| super::sqlx_error("pairing.terminalize", error))?;
    rows.into_iter()
        .map(|row| {
            row.try_get::<String, _>("ephemeral_id")
                .map_err(|error| super::sqlx_error("pairing.terminalize", error))
        })
        .collect()
}

fn push_selection(statement: &mut QueryBuilder<Sqlite>, selector: LiveSelector) {
    match selector {
        LiveSelector::ById(id) => {
            statement.push(" WHERE id = ");
            statement.push_bind(id);
        }
        LiveSelector::ExpiredBy(bound) => {
            statement.push(" WHERE expires_at_ms <= ");
            statement.push_bind(bound);
        }
        LiveSelector::SameKey(key) => {
            statement.push(" WHERE public_key = ");
            statement.push_bind(key.to_vec());
        }
        LiveSelector::ExpiredByBatch { bound, limit } => {
            statement.push(
                " WHERE id IN (SELECT id FROM pair_requests \
                   WHERE status IN ('pending', 'verification_required') \
                     AND expires_at_ms <= ",
            );
            statement.push_bind(bound);
            statement.push(" ORDER BY expires_at_ms LIMIT ");
            statement.push_bind(limit);
            statement.push(")");
        }
    }
}

/// Terminalize one live request as `expired`, clearing its code digest.
pub async fn mark_expired<'a, E>(
    executor: E,
    row: &PairRequestRow,
    now_ms: i64,
) -> PairingResult<Vec<String>>
where
    E: sqlx::Executor<'a, Database = Sqlite>,
{
    terminalize(
        executor,
        LiveSelector::ById(row.id),
        TerminalRequest::Expired,
        now_ms,
    )
    .await
}

/// How many requests are live right now.
pub async fn count_live<'a, E>(executor: E) -> PairingResult<i64>
where
    E: sqlx::Executor<'a, Database = Sqlite>,
{
    let row = sqlx::query_as::<_, (i64,)>(
        "SELECT COUNT(*) FROM pair_requests \
          WHERE status IN ('pending', 'verification_required')",
    )
    .fetch_one(executor)
    .await
    .map_err(|error| super::sqlx_error("pairing.count", error))?;
    Ok(row.0)
}

/// Insert the `pending` row, refusing a key that was revoked in the meantime.
///
/// The revocation is re-tested inside the insert's own `NOT EXISTS` rather than
/// only in the read above it, so a revocation committed between the two cannot
/// slip a request past the check that was supposed to fence it. The ceiling is
/// re-tested there for the same reason.
pub async fn insert_request<'a, E>(
    executor: E,
    input: &PairRequestCreate<'_>,
    requester_fingerprint: &str,
) -> PairingResult<()>
where
    E: sqlx::Executor<'a, Database = Sqlite>,
{
    let affected = sqlx::query(
        "INSERT INTO pair_requests ( \
             id, ephemeral_id, public_key, label, status, created_at_ms, decided_at_ms, \
             ceremony_version, requester_token_hash, verification_code_hash, \
             verification_attempts, approved_by_fp, approved_account_id, \
             user_agent, client_browser, client_os, client_device_type, source_ip, \
             country_code, region, city, edge_identity_provider, edge_identity, \
             edge_identity_verified, expires_at_ms) \
         SELECT ?, ?, ?, ?, 'pending', ?, NULL, ?, ?, NULL, 0, NULL, NULL, \
                ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ? \
          WHERE NOT EXISTS ( \
              SELECT 1 FROM authorized_key_revocations WHERE fingerprint = ?) \
            AND (SELECT COUNT(*) FROM pair_requests \
                  WHERE status IN ('pending', 'verification_required')) < ?",
    )
    .bind(input.ephemeral_id)
    .bind(input.ephemeral_id)
    .bind(input.public_key.to_vec())
    .bind(input.label)
    .bind(input.now_ms)
    .bind(i64::from(PAIRING_CEREMONY_VERSION))
    .bind(input.requester_token_hash)
    .bind(&input.provenance.user_agent)
    .bind(&input.provenance.client_browser)
    .bind(&input.provenance.client_os)
    .bind(
        input
            .provenance
            .client_device_type
            .map(ClientDeviceType::as_wire),
    )
    .bind(input.provenance.source_ip())
    .bind(&input.provenance.country_code)
    .bind(&input.provenance.region)
    .bind(&input.provenance.city)
    .bind(input.edge_identity_provider)
    .bind(input.edge_identity)
    .bind(i64::from(input.edge_identity.is_some()))
    .bind(input.expires_at_ms)
    .bind(requester_fingerprint)
    .bind(MAX_PENDING_PAIR_REQUESTS)
    .execute(executor)
    .await
    .map_err(|error| super::sqlx_error("pairing.insert", error))?
    .rows_affected();
    if affected != 1 {
        return Err(refuse(PairingRefusal::TooManyLiveRequests));
    }
    Ok(())
}

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
    E: sqlx::Executor<'a, Database = Sqlite>,
{
    let row = sqlx::query_as::<_, PairRequestColumns>(
        "SELECT id, ephemeral_id, status, ceremony_version, expires_at_ms, label, \
                requester_token_hash, public_key, approved_account_id, approved_by_fp, \
                verification_code_hash, verification_attempts \
           FROM pair_requests WHERE ephemeral_id = ?",
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
    E: sqlx::Executor<'a, Database = Sqlite>,
{
    let row = sqlx::query_as::<_, PairRequestColumns>(
        "SELECT id, ephemeral_id, status, ceremony_version, expires_at_ms, label, \
                requester_token_hash, public_key, approved_account_id, approved_by_fp, \
                verification_code_hash, verification_attempts \
           FROM pair_requests \
          WHERE ephemeral_id = ? AND status IN ('pending', 'verification_required')",
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
    id: i64,
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
