//! The reads, projections and gate decisions the seven `Pair*` handlers
//! share. Owned by the pairing slice; called only by
//! [`crate::auth::rpc_pairing`].
//!
//! These live below the RPC surface rather than in it because each one is a
//! fact about the ceremony rather than about a method: which rows a list shows,
//! what a bus frame may carry, and when a caller is an approver at all. A
//! handler that inlined them would make each of those three questions a
//! per-method answer.

use connectrpc::{ConnectError, ErrorCode, RequestContext};

use super::confirmation::PairConfirmation;
use super::provenance::PairRequestProvenance;
use super::rows::{self, LiveSelector};
use super::status::{ApprovalStatusFacts, StoredStatus, TerminalRequest};
use super::{PairingError, PairingRefusal, sqlx_error};
use crate::auth::cf_access::CloudflareAccessIdentity;
use crate::coord_core::{Caller, CoordCore, ListenerTrust};
use crate::middleware::caller_origin::{CallerOrigin, resolve_caller_origin};
use crate::db::CoordDb;
use crate::events::bus_messages::PairRequestDelta;
use crate::write_gate::SharedLease;

/// What a caller is told when the exclusive keeper-update drain holds the gate.
/// One message for both variants: a client cannot act on which one it was.
const WRITE_GATE_HELD_MESSAGE: &str = "coordinator keeper update preparation in progress";

/// A shared write lease, or the refusal the write gate throws.
///
/// Held across the durable half of a mutation, not the whole handler: a
/// `PairCreate` that spends its time validating wire values and hashing a token
/// has not touched the database yet, and holding the gate through that is a
/// gate that serializes every pairing request against every keystroke.
pub(crate) fn lease(core: &CoordCore) -> Result<SharedLease, ConnectError> {
    core.services
        .write_gate()
        .acquire_shared()
        // A FIXED LITERAL, not `WriteGateError`'s Display. Both of its variants
        // mean the same thing to a client, and forwarding another module's error
        // text puts this domain's wire messages at the mercy of a type it does
        // not own -- and every internal error here renders as `"{field}: {reason}"`.
        .map_err(|_| ConnectError::new(ErrorCode::Unavailable, WRITE_GATE_HELD_MESSAGE))
}

/// The caller's origin for this request, as the origin layer resolved it.
///
/// DELEGATES to `middleware::caller_origin::resolve_caller_origin`. That
/// resolver owns the rule that matters: under a trusted proxy, the mere PRESENCE
/// of `X-Forwarded-For` proves a proxy was traversed, which disqualifies the
/// request from on-host authority even when the address behind it is loopback.
pub(crate) fn caller_origin_of(context: &RequestContext) -> CallerOrigin {
    resolve_caller_origin(
        observed_trust(context),
        context.peer_addr().map(|address| address.to_string()).as_deref(),
        context
            .headers()
            .get("x-forwarded-for")
            .and_then(|value| value.to_str().ok()),
    )
}

/// The trust profile the listener under this request claims.
///
/// Read from the header the listener sets, never sniffed: a caller that could
/// choose its own trust profile could choose to be trusted.
fn observed_trust(context: &RequestContext) -> ListenerTrust {
    match context
        .headers()
        .get("x-roost-listener-trust")
        .and_then(|value| value.to_str().ok())
    {
        Some("trusted-proxy") => ListenerTrust::Forwarded,
        _ => ListenerTrust::DirectLoopback,
    }
}

/// Whether this caller may act as an approver, and which fingerprint to record.
///
/// A browser key is authority wherever it connected from; a direct on-host
/// caller is authority only when it presented no key at all, because a worker
/// or a revoked key that reached the loopback listener is exactly the case the
/// on-host gate exists to keep out.
pub(crate) fn approver_or_on_host(caller: &Caller) -> Result<Option<&str>, ConnectError> {
    if caller.principal.is_browser() {
        return Ok(Some(caller.fingerprint()));
    }
    if caller.on_host {
        return Ok(None);
    }
    Err(ConnectError::new(
        ErrorCode::PermissionDenied,
        PairingRefusal::OnHostOnly.to_string(),
    ))
}

/// The browser fingerprint of a caller that has one, and `None` otherwise.
pub(crate) fn optional_fingerprint(caller: &Caller) -> Option<&str> {
    caller.principal.is_browser().then(|| caller.fingerprint())
}

/// The verified front-door identity for this request, or `None` when the
/// deployment has no front door.
///
/// An on-host caller never reaches the verifier, which is the property that
/// makes a direct connection keep working when the edge in front of it is
/// misconfigured: a coordinator an operator is sitting at must not be
/// unpairable because somebody else broke their proxy.
pub(crate) async fn front_door_identity(
    core: &CoordCore,
    context: &RequestContext,
    on_host: bool,
) -> Result<Option<CloudflareAccessIdentity>, ConnectError> {
    if on_host {
        return Ok(None);
    }
    // `require_config` already answers with a `ConnectError`, and it is a
    // better one than anything re-wrapping could produce: it carries the
    // status, and re-encoding it through `to_string()` would both drop its
    // headers and fold the code into the message as `internal: <reason>`.
    let config = core.services.boot.require_config()?;
    crate::auth::cf_access::verify_edge_identity(
        config,
        context.headers(),
        crate::auth::jwt_verify::VerifyClock::at(crate::rpc::service::now_ms()),
    )
    .await
    .map_err(|rejection| {
        tracing::info!(reason = %rejection, "a pairing request carried no front-door identity");
        ConnectError::new(
            ErrorCode::Unauthenticated,
            PairingRefusal::FrontDoorSignInRequired.to_string(),
        )
    })
}

/// What `PairPoll` is allowed to see about a request.
///
/// Deliberately two columns. The poll's whole contract is "your token, your
/// status", and a projection that carried the label or the approver's
/// fingerprint would be a projection whose callers could be tempted to display.
pub(crate) struct PollRow {
    /// The stored status.
    pub(crate) status: String,
    /// When the request stops being redeemable.
    pub(crate) expires_at_ms: i64,
}

/// The row a `PairPoll` may read: the one whose id AND requester token digest
/// both match, and whose token digest is not empty.
///
/// The digest is a `WHERE` term rather than a comparison afterwards, so a row
/// that exists under a different token is never read at all.
pub(crate) async fn read_under_token(
    database: &CoordDb,
    ephemeral_id: &str,
    requester_token_hash: &str,
) -> Result<PollRow, ConnectError> {
    let row = sqlx::query_as::<_, (String, i64)>(
        "SELECT status, expires_at_ms FROM pair_requests \
          WHERE ephemeral_id = ? AND requester_token_hash = ? \
            AND requester_token_hash != ''",
    )
    .bind(ephemeral_id)
    .bind(requester_token_hash)
    .fetch_optional(database.pool())
    .await
    .map_err(|error| sqlx_error("pairing.poll", error))
    .map_err(PairingError::into_error)?;
    row.map(|(status, expires_at_ms)| PollRow {
        status,
        expires_at_ms,
    })
    .ok_or_else(|| PairingRefusal::NotFound.into_error())
}

/// The pending requests an approver is shown, newest first.
///
/// `pending` only, and only unexpired ones: a request past its deadline is
/// already refused by Approve and by Confirm, so listing it would be an
/// operator being offered a choice that cannot succeed.
pub(crate) async fn list_pending(
    database: &CoordDb,
    now_ms: i64,
) -> Result<Vec<roost_proto::PairRequest>, ConnectError> {
    let rows = sqlx::query_as::<_, PendingColumns>(
        "SELECT ephemeral_id, label, created_at_ms, user_agent, client_browser, client_os, \
                client_device_type, source_ip, country_code, region, city, \
                edge_identity_provider, edge_identity, edge_identity_verified, expires_at_ms \
           FROM pair_requests WHERE status = 'pending' AND expires_at_ms > ? \
          ORDER BY created_at_ms DESC",
    )
    .bind(now_ms)
    .fetch_all(database.pool())
    .await
    .map_err(|error| sqlx_error("pairing.list", error))
    .map_err(PairingError::into_error)?;
    Ok(rows.into_iter().map(PendingColumns::into_proto).collect())
}

/// The `PairList` projection, named so a column cannot shift its neighbours.
#[derive(Debug, sqlx::FromRow)]
struct PendingColumns {
    ephemeral_id: String,
    label: String,
    created_at_ms: i64,
    user_agent: Option<String>,
    client_browser: Option<String>,
    client_os: Option<String>,
    client_device_type: Option<String>,
    source_ip: Option<String>,
    country_code: Option<String>,
    region: Option<String>,
    city: Option<String>,
    edge_identity_provider: Option<String>,
    edge_identity: Option<String>,
    edge_identity_verified: i64,
    expires_at_ms: i64,
}

impl PendingColumns {
    /// A `NULL` column as the empty string, because a proto3 `string` has one
    /// way to say "not known" and inventing a second one would show an operator
    /// the word `null` as though it were a browser name.
    fn into_proto(self) -> roost_proto::PairRequest {
        roost_proto::PairRequest {
            ephemeral_id: self.ephemeral_id,
            label: self.label,
            created_at_ms: unsigned(self.created_at_ms),
            user_agent: self.user_agent.unwrap_or_default(),
            client_browser: self.client_browser.unwrap_or_default(),
            client_os: self.client_os.unwrap_or_default(),
            client_device_type: self.client_device_type.unwrap_or_default(),
            source_ip: self.source_ip.unwrap_or_default(),
            country_code: self.country_code.unwrap_or_default(),
            region: self.region.unwrap_or_default(),
            city: self.city.unwrap_or_default(),
            edge_identity_provider: self.edge_identity_provider.unwrap_or_default(),
            edge_identity: self.edge_identity.unwrap_or_default(),
            edge_identity_verified: self.edge_identity_verified != 0,
            expires_at_ms: unsigned(self.expires_at_ms),
            ..Default::default()
        }
    }
}

/// The three columns `PairApprovalStatus` needs, and nothing more.
pub(crate) async fn read_status_facts(
    database: &CoordDb,
    ephemeral_id: &str,
) -> super::PairingResult<Option<ApprovalStatusFacts>> {
    let row = sqlx::query_as::<_, (String, Option<String>, i64)>(
        "SELECT status, approved_by_fp, expires_at_ms FROM pair_requests WHERE ephemeral_id = ?",
    )
    .bind(ephemeral_id)
    .fetch_optional(database.pool())
    .await
    .map_err(|error| sqlx_error("pairing.status", error))?;
    Ok(match row {
        Some((status, approved_by_fp, expires_at_ms)) => Some(ApprovalStatusFacts {
            status: StoredStatus::parse(&status)?,
            approved_by_fingerprint: approved_by_fp,
            expires_at_ms,
        }),
        None => None,
    })
}

/// Deny one live request by its ceremony handle.
///
/// Resolves the handle to a surrogate key first rather than adding a
/// handle-shaped selector to `account`: every other transition in this domain
/// is addressed the way the ceremony addresses a row, and one path that is
/// addressed the way a caller names it is one path whose addressing can drift.
pub(crate) async fn deny_request(
    core: &CoordCore,
    ephemeral_id: &str,
    now_ms: i64,
) -> Result<(), ConnectError> {
    let row = rows::read_live_pair_request(core.services.db.pool(), ephemeral_id)
        .await
        .map_err(PairingError::into_error)?
        .ok_or_else(|| PairingRefusal::NotFound.into_error())?;
    if !row.status.is_live() {
        return Err(PairingRefusal::NotPending.into_error());
    }
    let removed = rows::terminalize(
        core.services.db.pool(),
        LiveSelector::ById(row.id),
        TerminalRequest::Denied,
        now_ms,
    )
    .await
    .map_err(PairingError::into_error)?;
    if removed.is_empty() {
        return Err(PairingRefusal::NotPending.into_error());
    }
    Ok(())
}

/// Publish a request leaving the pending set.
pub(crate) fn publish_removed(core: &CoordCore, ephemeral_id: &str) {
    core.services
        .buses
        .pair_bus
        .publish(PairRequestDelta::Removed {
            ephemeral_id: ephemeral_id.to_string(),
        });
}

/// Publish a new pending request, with descriptors and nothing else.
///
/// The frame carries no key material, no requester token and no code digest,
/// because it reaches every connected browser -- not only the one that posted
/// it.
pub(crate) fn publish_pending(
    core: &CoordCore,
    ephemeral_id: &str,
    label: &str,
    now_ms: i64,
    observed: &PairRequestProvenance,
    edge: Option<&CloudflareAccessIdentity>,
) {
    let expires_at_ms = now_ms + super::secrets::PAIR_REQUEST_TTL_MS;
    core.services
        .buses
        .pair_bus
        .publish(PairRequestDelta::Pending {
            ephemeral_id: ephemeral_id.to_string(),
            label: label.to_string(),
            created_at_ms: now_ms,
            user_agent: observed.user_agent.clone().unwrap_or_default(),
            client_browser: observed.client_browser.clone().unwrap_or_default(),
            client_os: observed.client_os.clone().unwrap_or_default(),
            client_device_type: observed
                .client_device_type
                .map_or(String::new(), |kind| kind.as_wire().to_string()),
            source_ip: observed.source_ip().to_string(),
            country_code: observed.country_code.clone().unwrap_or_default(),
            region: observed.region.clone().unwrap_or_default(),
            city: observed.city.clone().unwrap_or_default(),
            edge_identity_provider: edge
                .map_or(String::new(), |identity| identity.provider().to_string()),
            edge_identity: edge.map_or(String::new(), |identity| identity.email().to_string()),
            edge_identity_verified: edge.is_some(),
            expires_at_ms,
        });
}

/// Report what a confirmation did: the bus notice, the log line, and the one
/// authorization side effect a completed confirmation owes.
pub(crate) fn report_confirmation(
    core: &CoordCore,
    ephemeral_id: &str,
    result: &PairConfirmation,
) {
    if let Some(status) = result.terminal_status {
        tracing::info!(
            ephemeral_id,
            terminal = status.as_wire(),
            "a pair confirmation ended the request"
        );
    }
    if let Some(notice) = &result.paired_browser {
        core.services
            .buses
            .pair_bus
            .publish(PairRequestDelta::Completed {
                ephemeral_id: notice.ephemeral_id.clone(),
                label: notice.label.clone(),
                client_browser: notice.client_browser.clone(),
                client_os: notice.client_os.clone(),
                client_device_type: notice.client_device_type.clone(),
                country_code: notice.country_code.clone(),
                region: notice.region.clone(),
                city: notice.city.clone(),
                paired_at_ms: notice.paired_at_ms,
            });
    }
    if let Some(fingerprint) = &result.newly_authorized_fingerprint {
        // `refresh_jwt_key`, not an invalidation: the key was just authorized,
        // so a verifier that already loaded the row must stay valid and the
        // confirming browser's very next RPC must find the new one. This is
        // v2's `refreshJwtKey` (`handlers-pairing.ts:289`).
        core.services.jwt_keys.refresh_jwt_key(fingerprint);
        tracing::info!(
            ephemeral_id,
            fp = %fingerprint,
            "a browser completed pairing and its key is now authorized"
        );
    }
}

/// Epoch milliseconds into a proto `uint64`, clamping a negative rather than
/// wrapping it into a value four centuries in the future.
pub(crate) fn unsigned(millis: i64) -> u64 {
    u64::try_from(millis).unwrap_or_default()
}
