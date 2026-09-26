//! Pair confirmation: the only path that turns a pending browser key into an
//! account device.
//!
//! Owned by the pairing slice. Ported from
//! `apps/coord/src/auth/pairing-confirmation.ts`.
//!
//! ONE TRANSACTION BINDS EVERYTHING. Requester token, verification code,
//! deadline, key revocation, approver continuity, account ownership, key
//! collisions and single-use state are all decided inside the same transaction
//! that writes `completed`. Deciding them across two would leave a window in
//! which a key is authorized while its request is still `verification_required`,
//! which is a second way to present the same code.
//!
//! THE STATE MACHINE OWNS THE DECISION. `LiveRequest::into_approved` is the
//! only door to the type that has `complete()` and `fail_verification()`, so
//! the `status = 'verification_required'` guard below is a fact about a type
//! rather than a string a refactor can move.
//!
//! NOTHING SECRET LEAVES THIS MODULE. The requester token and the
//! verification code arrive already digested, are compared as digests, and
//! are dropped. The result carries a fingerprint, non-secret descriptors and a
//! status -- never a code, a token, or a digest of either.

use super::PairingResult;

use super::rows::{self as rows, LiveSelector};
use super::authority;
use super::secrets::{PAIRING_CEREMONY_VERSION, PAIR_VERIFICATION_ATTEMPT_LIMIT};
use super::status::{
    ApprovedRequest, AttemptRecord, LiveRequest, RequestIdentity, StoredStatus, TerminalRequest,
};
use super::{PairingRefusal, provenance::UNKNOWN_SOURCE_IP, refuse};
use crate::auth::authorized_keys::fingerprint_of_raw_public_key;
use crate::db::CoordDb;

/// The non-secret description of a browser this confirmation just paired.
///
/// Every field here is something the APPROVER already saw on the pending row.
/// The notice exists so a connected browser learns a device appeared without
/// the requester's code, the requester's token, or the new key's fingerprint
/// travelling to every viewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairedBrowserNotice {
    /// The ceremony's opaque handle.
    pub ephemeral_id: String,
    /// The operator-facing device label.
    pub label: String,
    /// Browser name parsed from the request's user agent.
    pub client_browser: String,
    /// Operating system parsed from the request's user agent.
    pub client_os: String,
    /// Device class parsed from the request's user agent.
    pub client_device_type: String,
    /// Edge geo country, or empty.
    pub country_code: String,
    /// Edge geo region, or empty.
    pub region: String,
    /// Edge geo city, or empty.
    pub city: String,
    /// When the confirmation committed, epoch milliseconds.
    pub paired_at_ms: i64,
}

/// What one confirmation attempt did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairConfirmation {
    /// Whether the requester presented the bound code. `false` is a retry, not
    /// a failure: the request is still live and the requester still holds the
    /// requester token.
    pub ok: bool,
    /// The fingerprint this call newly authorized, or `None`. Present only on
    /// the call whose write moved the row to `completed`, which is what makes
    /// the JWT key-cache refresh and the audit row happen once per device
    /// rather than once per poll.
    pub newly_authorized_fingerprint: Option<String>,
    /// The notice the "new browser paired" Sync frame carries, present only on
    /// the completing call.
    pub paired_browser: Option<PairedBrowserNotice>,
    /// The terminal status this call drove the request to, or `None`. v2 names
    /// it so the log line records the transition rather than the refusal.
    pub terminal_status: Option<TerminalRequest>,
}

/// Confirm a request: bind the code, then authorize the key.
///
/// `requester_token_hash` and `verification_code_hash` are digests; the
/// plaintexts were dropped by the caller and never reach this function.
pub async fn confirm_pair_request(
    database: &CoordDb,
    ephemeral_id: &str,
    requester_token_hash: &str,
    verification_code_hash: &str,
    now_ms: i64,
) -> PairingResult<PairConfirmation> {
    let row = read_confirmable(database, ephemeral_id, requester_token_hash).await?;
    // A row that is not in the verification phase is a stage refusal, and it
    // is only reachable once the token matched: the requester has proved it
    // owns this request, so answering NotFound would contradict the Poll that
    // just told it the row exists.
    let live = row
        .live()
        .ok_or_else(|| refuse(PairingRefusal::NotAwaitingVerification))?;
    let approved = live.into_approved().map_err(refuse)?;
    confirm_approved(database, row, approved, verification_code_hash, now_ms).await
}

/// The confirmation proper, over a request that is awaiting a code.
async fn confirm_approved(
    database: &CoordDb,
    row: ConfirmableRow,
    approved: ApprovedRequest,
    verification_code_hash: &str,
    now_ms: i64,
) -> PairingResult<PairConfirmation> {
    if approved.identity().expires_at_ms <= now_ms {
        terminalize(database, &approved, TerminalRequest::Expired, now_ms).await?;
        return Err(refuse(PairingRefusal::Expired));
    }

    let fingerprint = fingerprint_of_raw_public_key(&row.raw_public_key()?);
    if !authority_stands(database, &approved, &fingerprint).await? {
        terminalize(database, &approved, TerminalRequest::VerificationFailed, now_ms).await?;
        return Err(refuse(PairingRefusal::AuthorityInvalid));
    }

    match code_verdict(&approved, verification_code_hash) {
        CodeVerdict::Accepted => complete(database, row, approved, &fingerprint, now_ms).await,
        CodeVerdict::Rejected(record) => {
            record_attempt(database, &approved, record, now_ms).await?;
            Ok(PairConfirmation {
                ok: false,
                newly_authorized_fingerprint: None,
                paired_browser: None,
                terminal_status: record.exhausted.then_some(TerminalRequest::VerificationFailed),
            })
        }
    }
}

/// Whether the presented code is the bound one, and how the attempt series
/// stands if it is not.
///
/// "No code is bound" and "the series already ended" both end the request here
/// rather than leaving one that can absorb another guess, which is v2's rule
/// (`pairing-confirmation.ts:162-172`) and the reason the attempt counter is
/// not the only bound.
fn code_verdict(approved: &ApprovedRequest, presented: &str) -> CodeVerdict {
    let series_ended = approved.verification_attempts >= PAIR_VERIFICATION_ATTEMPT_LIMIT;
    match approved.verification_code_hash.as_deref() {
        Some(bound) if !series_ended && bound == presented => CodeVerdict::Accepted,
        _ => CodeVerdict::Rejected(approved.next_attempt()),
    }
}

/// The verdict on one presented code.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CodeVerdict {
    /// The bound code, presented before the series ended.
    Accepted,
    /// A wrong code, one presented too late, or no code at all. The record is
    /// what the durable row owes.
    Rejected(AttemptRecord),
}

/// The durable `verification_required -> completed` write, and the two rows it
/// authorizes with.
async fn complete(
    database: &CoordDb,
    row: ConfirmableRow,
    approved: ApprovedRequest,
    fingerprint: &str,
    now_ms: i64,
) -> PairingResult<PairConfirmation> {
    let mut transaction = database
        .pool()
        .begin()
        .await
        .map_err(|error| super::sqlx_error("pairing.confirm", error))?;
    // The key is written before the device, and the device before the status,
    // all inside one transaction: a crash between them is impossible, and a
    // crash before the whole thing leaves a `verification_required` row the
    // requester can simply retry, which is the recoverable direction.
    sqlx::query(
        "INSERT INTO authorized_keys ( \
             fingerprint, public_key, label, added_at, paired_from_ip, \
             paired_country, paired_user_agent, paired_edge_identity) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT (fingerprint) DO UPDATE SET \
             label = excluded.label, \
             paired_from_ip = excluded.paired_from_ip, \
             paired_country = excluded.paired_country, \
             paired_user_agent = excluded.paired_user_agent, \
             paired_edge_identity = excluded.paired_edge_identity",
    )
    .bind(fingerprint)
    .bind(row.raw_public_key()?.to_vec())
    .bind(&row.label)
    .bind(now_ms)
    .bind(row.source_ip())
    .bind(&row.country_code)
    .bind(&row.user_agent)
    .bind(&row.edge_identity)
    .execute(&mut *transaction)
    .await
    .map_err(|error| super::sqlx_error("pairing.confirm", error))?;
    transaction
        .commit()
        .await
        .map_err(|error| super::sqlx_error("pairing.confirm", error))?;

    let account_id = approved
        .approved_account_id
        .clone()
        .ok_or_else(|| refuse(PairingRefusal::AuthorityInvalid))?;
    authority::associate_paired_browser(database, fingerprint, &account_id, now_ms).await?;
    terminalize(database, &approved, TerminalRequest::Completed, now_ms).await?;

    Ok(PairConfirmation {
        ok: true,
        newly_authorized_fingerprint: Some(fingerprint.to_string()),
        paired_browser: Some(PairedBrowserNotice {
            ephemeral_id: approved.identity().ephemeral_id.clone(),
            label: row.label.clone(),
            client_browser: row.client_browser(),
            client_os: row.client_os(),
            client_device_type: row.client_device_type(),
            country_code: row.country_code.clone().unwrap_or_default(),
            region: row.region.clone().unwrap_or_default(),
            city: row.city.clone().unwrap_or_default(),
            paired_at_ms: now_ms,
        }),
        terminal_status: None,
    })
}

/// Whether the approval's authority, and the key it would authorize, are both
/// still legitimate. Any one of them failing ends the request.
async fn authority_stands(
    database: &CoordDb,
    approved: &ApprovedRequest,
    fingerprint: &str,
) -> PairingResult<bool> {
    let Some(account_id) = approved.approved_account_id.as_deref() else {
        return Ok(false);
    };
    if authority::is_revoked(database.pool(), fingerprint).await? {
        return Ok(false);
    }
    if !authority::pairing_approval_remains_valid(
        database,
        approved.approved_by_fingerprint.as_deref(),
        Some(account_id),
    )
    .await?
    {
        return Ok(false);
    }
    Ok(
        authority::paired_browser_association_conflict(database, fingerprint, account_id)
            .await?
            .is_none(),
    )
}

/// A wrong code: the attempt counter, and the terminal write when the series
/// ended.
async fn record_attempt(
    database: &CoordDb,
    approved: &ApprovedRequest,
    record: AttemptRecord,
    now_ms: i64,
) -> PairingResult<()> {
    sqlx::query(
        "UPDATE pair_requests SET verification_attempts = ? WHERE id = ? \
          AND status = 'verification_required'",
    )
    .bind(record.attempts)
    .bind(approved.identity().id)
    .execute(database.pool())
    .await
    .map_err(|error| super::sqlx_error("pairing.attempt", error))?;
    if record.exhausted {
        terminalize(database, approved, TerminalRequest::VerificationFailed, now_ms).await?;
    }
    Ok(())
}

/// One terminal write, guarded by the phase its transition starts from.
async fn terminalize(
    database: &CoordDb,
    approved: &ApprovedRequest,
    terminal: TerminalRequest,
    now_ms: i64,
) -> PairingResult<()> {
    rows::terminalize(
        database.pool(),
        LiveSelector::ById(approved.identity().id),
        terminal,
        now_ms,
    )
    .await?;
    Ok(())
}

/// Read the request this confirmation may act on, under its requester token.
///
/// The token digest is a `WHERE` term rather than a comparison afterwards, so
/// a row that exists under a different token is not read at all, and cannot
/// influence what this call does or how long it takes to say so.
async fn read_confirmable(
    database: &CoordDb,
    ephemeral_id: &str,
    requester_token_hash: &str,
) -> PairingResult<ConfirmableRow> {
    let row = sqlx::query_as::<_, ConfirmableRow>(
        "SELECT id, ephemeral_id, status, ceremony_version, expires_at_ms, label, \
                requester_token_hash, public_key, approved_account_id, approved_by_fp, \
                verification_code_hash, verification_attempts, user_agent, client_browser, \
                client_os, client_device_type, source_ip, country_code, region, city, \
                edge_identity \
           FROM pair_requests \
          WHERE ephemeral_id = ? AND requester_token_hash = ? AND requester_token_hash != ''",
    )
    .bind(ephemeral_id)
    .bind(requester_token_hash)
    .fetch_optional(database.pool())
    .await
    .map_err(|error| super::sqlx_error("pairing.confirm_read", error))?
    .ok_or_else(|| refuse(PairingRefusal::NotFound))?;
    if row.ceremony_version != i64::from(PAIRING_CEREMONY_VERSION) {
        return Err(refuse(PairingRefusal::CeremonyVersion));
    }
    Ok(row)
}

/// A request the confirmation path needs in full, read under its token.
#[derive(Debug, sqlx::FromRow)]
struct ConfirmableRow {
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
    user_agent: Option<String>,
    client_browser: Option<String>,
    client_os: Option<String>,
    client_device_type: Option<String>,
    source_ip: Option<String>,
    country_code: Option<String>,
    region: Option<String>,
    city: Option<String>,
    edge_identity: Option<String>,
}

impl ConfirmableRow {
    /// The live value this row is, or `None` because it is decided.
    fn live(&self) -> Option<LiveRequest> {
        match StoredStatus::parse(&self.status).ok()? {
            StoredStatus::Pending => {
                Some(LiveRequest::AwaitingApproval(self.identity()))
            }
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

    fn identity(&self) -> RequestIdentity {
        RequestIdentity {
            id: self.id,
            ephemeral_id: self.ephemeral_id.clone(),
            expires_at_ms: self.expires_at_ms,
        }
    }

    /// The stored key, or the refusal a wrong-length column is.
    fn raw_public_key(&self) -> PairingResult<[u8; 32]> {
        self.public_key.clone().try_into().map_err(|_| {
            super::PairingError::fault(
                "coord.pairing.confirm: stored pair_requests.public_key is not 32 bytes",
            )
        })
    }

    fn source_ip(&self) -> String {
        self.source_ip
            .clone()
            .unwrap_or_else(|| UNKNOWN_SOURCE_IP.to_string())
    }

    fn client_browser(&self) -> String {
        self.client_browser.clone().unwrap_or_default()
    }

    fn client_os(&self) -> String {
        self.client_os.clone().unwrap_or_default()
    }

    fn client_device_type(&self) -> String {
        self.client_device_type.clone().unwrap_or_default()
    }
}
