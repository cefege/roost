//! The MCP relay registry: the four Connect methods that say which external
//! servers an agent may call, and the opaque payloads published against them.
//! Ported from `apps/coord/src/sessions/handlers-mcp.ts`; every method is wired
//! from [`METHOD_HANDLERS`] and is a device-authenticated read or write of the
//! `mcp_relays` table plus, for the mutations that have a delta, one publish onto
//! `core.services.buses.mcp_bus`. The rows are the domain's only state, so this
//! module holds none and is reached entirely through `core.services`.

use std::io::Read as _;
use std::time::Duration;

use connectrpc::{ConnectError, ErrorCode, ServiceResult};
use roost_proto as proto;
use roost_proto::buffa::MessageField;
use roost_protocol::wire::{
    McpRelay, McpRelayDelta, McpRelayEvent, McpRelayId, McpRelayKind, McpStreamMessage,
};
use serde_json::{Map, Value};
use sqlx::Row as _;

use crate::coord_core::{Caller, CoordCore};
use crate::rpc::service::{now_ms, ok_response};

/// `CoordinatorService.McpList` — every relay this dashboard holds.
///
/// The scope is the dashboard rather than "all rows", so the one place a relay
/// id is resolved is stated once instead of per statement. v2 left it unscoped
/// because a self-hosted install has exactly one dashboard
/// (`auth/self-hosted-tenant.rs` admits a second as a misconfiguration). The
/// order is stated for the same reason v2 left it to SQLite, which answers a
/// bare `SELECT` in whatever order storage hands back: the pane sorts
/// client-side, so no consumer depended on it, but a stable answer is the
/// difference between a caller that can rely on it and one that cannot.
pub async fn handle_mcp_list(
    core: &CoordCore,
    caller: &Caller,
    _request: proto::McpListRequest,
) -> ServiceResult<proto::McpListResponse> {
    require_account_device(caller)?;
    let dashboard = dashboard_id(core)?;
    let query = sqlx::query_as::<_, StoredRelay>(LIST_RELAYS).bind(dashboard);
    let rows = within_store_deadline("McpList", query.fetch_all(core.services.db.pool())).await?;
    let relays = rows
        .into_iter()
        .map(StoredRelay::into_proto)
        .collect::<Result<Vec<_>, _>>()?;
    ok_response(proto::McpListResponse {
        relays,
        ..Default::default()
    })
}

/// One `mcp_relays` row, exactly as the column holds it.
#[derive(Debug, sqlx::FromRow)]
struct StoredRelay {
    id: String,
    label: String,
    kind: String,
    config_json: String,
    created_at_ms: i64,
}

impl StoredRelay {
    /// The wire relay a row answers with, from the column's bytes unparsed: v2
    /// never looks at the config, and a registry that can fail to list because
    /// one hand-edited row's JSON is malformed is not a registry.
    fn into_proto(self) -> Result<proto::McpRelay, ConnectError> {
        // A negative instant cannot come from `now_ms`; it refuses rather than
        // wrapping to a number no date has, which is what a cast would ship.
        let at = u64::try_from(self.created_at_ms)
            .map_err(|_| internal(format!("negative created_at_ms on relay {}", self.id)))?;
        Ok(proto::McpRelay {
            id: self.id,
            label: self.label,
            kind: self.kind,
            config_json: self.config_json,
            created_at_ms: at,
            ..Default::default()
        })
    }
}

/// `CoordinatorService.McpCreate` — register a relay and announce it.
///
/// THE CONFIG IS PARSED BEFORE THE INSERT, and that ordering is the reason it is
/// spelled here rather than inside the delta: the `created` delta published
/// after the commit carries the PARSED config, so a row whose config cannot be
/// parsed would leave a persisted relay the announcement cannot describe — the
/// RPC 500s, the row stays, and the SPA shows the prior state until a manual
/// refresh (`docs/FAILURE-INDEX.md`, "JSON.parse inside a bus publish, after the
/// commit"). The two representations are not interchangeable: the RPC echoes the
/// caller's bytes, the stream carries the object a worker reads.
pub async fn handle_mcp_create(
    core: &CoordCore,
    caller: &Caller,
    request: proto::McpCreateRequest,
) -> ServiceResult<proto::McpCreateResponse> {
    require_account_device(caller)?;
    let dashboard = dashboard_id(core)?;
    if request.label.is_empty() {
        return Err(invalid("label is required".to_owned()));
    }
    let config = parse_relay_config(&request.config_json)?;
    let kind = parse_relay_kind(&request.kind)?;
    let id = mint_relay_id()?;
    let row = StoredRelay {
        id: id.as_str().to_owned(),
        label: request.label.clone(),
        kind: kind.as_str().to_owned(),
        config_json: request.config_json.clone(),
        created_at_ms: now_ms(),
    };
    let insert = sqlx::query(INSERT_RELAY)
        .bind(&row.id)
        .bind(&row.label)
        .bind(&row.kind)
        .bind(&row.config_json)
        .bind(row.created_at_ms)
        .bind(dashboard);
    within_store_deadline("McpCreate", insert.execute(core.services.db.pool())).await?;
    core.services.buses.mcp_bus.publish(McpStreamMessage::Delta(McpRelayDelta::Created {
        relay: McpRelay {
            id: id.clone(),
            label: row.label.clone(),
            kind,
            config,
            created_at_ms: row.created_at_ms,
        },
    }));
    tracing::info!(relay_id = %id, kind = kind.as_str(), dashboard, "mcp: relay registered");
    ok_response(proto::McpCreateResponse {
        relay: MessageField::some(row.into_proto()?),
        ..Default::default()
    })
}

/// `CoordinatorService.McpDelete` — drop a relay this dashboard holds.
///
/// The delete and the existence decision are ONE statement (`DELETE ... RETURNING
/// id`); a select then a delete is two decisions with a window between them, and
/// the two-statement form also answers about a relay that stopped existing in
/// between.
///
/// A relay id this dashboard does not hold is `NotFound` and deletes nothing.
/// That is the boundary this surface has — the registry is install-wide, so a
/// delete cannot be aimed at a row the caller was never issued, and the refusal
/// is the same whether the id is unknown or held elsewhere, so a caller cannot
/// probe another dashboard's registry by watching which answer it gets.
pub async fn handle_mcp_delete(
    core: &CoordCore,
    caller: &Caller,
    request: proto::McpDeleteRequest,
) -> ServiceResult<proto::McpDeleteResponse> {
    require_account_device(caller)?;
    let dashboard = dashboard_id(core)?;
    let id = parse_relay_id(&request.id)?;
    let delete = sqlx::query(DELETE_RELAY)
        .bind(id.as_str())
        .bind(dashboard)
        .fetch_optional(core.services.db.pool());
    let removed = within_store_deadline("McpDelete", delete)
        .await?
        .map(removed_id)
        .transpose()?;
    let Some(removed) = removed else {
        return Err(relay_not_found());
    };
    let removed = McpRelayId::try_from(removed)
        .map_err(|error| internal(format!("mcp delete: removed id is not a uuid: {error}")))?;
    core.services.buses.mcp_bus.publish(McpStreamMessage::Delta(McpRelayDelta::Deleted {
        id: removed.clone(),
    }));
    tracing::info!(relay_id = %removed, dashboard, "mcp: relay removed");
    ok_response(proto::McpDeleteResponse {
        ok: true,
        ..Default::default()
    })
}

/// `CoordinatorService.McpPublish` — put one opaque payload on a relay's stream.
///
/// **"ACCEPTED" IS NOT "EXECUTED", AND THE COORDINATOR IS NOT THE PROXY.** The
/// third-party server behind a relay is contacted by the agent on a worker; the
/// coordinator persists the registry and relays payloads, which
/// `packages/protocol/src/wire/mcp.ts:1-2` states in one line ("Coord persists
/// rows; workers subscribe to the ... stream; payloads are opaque to coord"). So
/// `ok: true` means exactly one thing — the payload was accepted onto the
/// install-wide stream. It is not a delivery receipt, not a tool result, and not
/// an answer from the server: a relay whose server has died still accepts a
/// publish here, and the browser learns that from the call it issued against the
/// worker, the only component that can see the failure. What this method CAN
/// represent it refuses rather than fakes: a payload that is not JSON is
/// `InvalidArgument` before the publish, so an unrepresentable call can never
/// read back as an accepted one. An errored call and a call with no result are
/// not two states here — they are one state, on the worker.
pub async fn handle_mcp_publish(
    core: &CoordCore,
    caller: &Caller,
    request: proto::McpPublishRequest,
) -> ServiceResult<proto::McpPublishResponse> {
    require_account_device(caller)?;
    let dashboard = dashboard_id(core)?;
    let id = parse_relay_id(&request.id)?;
    let payload = parse_payload(&request.payload_json)?;
    let held = sqlx::query(RELAY_HELD)
        .bind(id.as_str())
        .bind(dashboard)
        .fetch_optional(core.services.db.pool());
    if within_store_deadline("McpPublish", held).await?.is_none() {
        return Err(relay_not_found());
    }
    let ts = now_ms();
    core.services.buses.mcp_bus.publish(McpStreamMessage::Event(McpRelayEvent {
        relay_id: id.clone(),
        payload,
        ts,
    }));
    tracing::info!(
        relay_id = %id,
        dashboard,
        "mcp: payload accepted; this is not an execution receipt"
    );
    ok_response(proto::McpPublishResponse {
        ok: true,
        ..Default::default()
    })
}

/// The Connect method each handler answers, and the function that answers it.
///
/// The lead's list: every row is one arm of the single `impl CoordinatorService`
/// block in `rpc/service_impl.rs`, so wiring this domain is reading this table
/// rather than matching on names by hand.
pub const METHOD_HANDLERS: &[(&str, &str)] = &[
    ("McpList", "sessions::mcp::handle_mcp_list"),
    ("McpCreate", "sessions::mcp::handle_mcp_create"),
    ("McpDelete", "sessions::mcp::handle_mcp_delete"),
    ("McpPublish", "sessions::mcp::handle_mcp_publish"),
];

/// How long one MCP statement may hold a caller before the call is refused.
///
/// [`crate::db::BUSY_TIMEOUT`] and not a number of this module's own. The
/// coordinator keeps ONE connection (`db.rs`), so every domain's mutation waits
/// on the same lock, and the crate has decided what that wait is worth: "above
/// the write gate's own hold time, so a mutation queued behind an exclusive
/// keeper-update drain waits rather than failing". A second, smaller number here
/// would be a second answer to that question, and the one a caller observed would
/// be whichever was smaller — a browser would start seeing MCP failures during a
/// deploy drain that workspaces and tasks ride out.
///
/// The timer wraps the whole statement, acquisition included, so the bound is on
/// the CALLER's wait rather than on a phase of it, and it turns a wedged store
/// from a request that never returns into a bounded refusal a browser can retry.
const STORE_DEADLINE: Duration = crate::db::BUSY_TIMEOUT;

/// Two statuses, because each tells the browser something different:
/// `Unavailable` means "the coordinator's own state did not answer; come back",
/// which a browser retries, and `Internal` means a statement failed, which it
/// does not. One status would either send a browser into a retry loop against a
/// broken database or make a retryable wait look permanent.
async fn within_store_deadline<T>(
    method: &'static str,
    work: impl Future<Output = Result<T, sqlx::Error>>,
) -> Result<T, ConnectError> {
    match tokio::time::timeout(STORE_DEADLINE, work).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(internal(error.to_string())),
        Err(_elapsed) => {
            tracing::error!(
                method,
                deadline_ms = STORE_DEADLINE.as_millis(),
                "mcp: the store did not answer inside the busy timeout; refusing rather than holding the caller"
            );
            Err(ConnectError::new(
                ErrorCode::Unavailable,
                format!(
                    "the coordinator store did not answer {method} within {}ms",
                    STORE_DEADLINE.as_millis()
                ),
            ))
        }
    }
}

/// The caller's own mistake — the one status worth separating from the
/// coordinator's, because the browser fixes it rather than retrying it.
fn invalid(reason: String) -> ConnectError {
    ConnectError::new(ErrorCode::InvalidArgument, reason)
}

/// A coordinator that is misassembled or whose store failed: both the
/// deployment's fault, neither the caller's to retry.
fn internal(reason: String) -> ConnectError {
    ConnectError::new(ErrorCode::Internal, reason)
}

/// The dashboard every relay read and write is scoped to.
fn dashboard_id(core: &CoordCore) -> Result<&str, ConnectError> {
    Ok(core.services.boot.require_tenant()?.dashboard_id.as_str())
}

const LIST_RELAYS: &str = "SELECT id, label, kind, config_json, created_at_ms FROM mcp_relays \
                           WHERE dashboard_id = ?1 ORDER BY created_at_ms, id";
const INSERT_RELAY: &str = "INSERT INTO mcp_relays \
                            (id, label, kind, config_json, created_at_ms, dashboard_id) \
                            VALUES (?1, ?2, ?3, ?4, ?5, ?6)";
const DELETE_RELAY: &str =
    "DELETE FROM mcp_relays WHERE id = ?1 AND dashboard_id = ?2 RETURNING id";
const RELAY_HELD: &str = "SELECT id FROM mcp_relays WHERE id = ?1 AND dashboard_id = ?2";

/// The `RETURNING id` column of a removed relay, as text.
///
/// The statement bound the id, so what comes back is what was bound; branding it
/// is the caller's job, and a row answering with anything else is reported
/// rather than papered over.
fn removed_id(row: sqlx::sqlite::SqliteRow) -> Result<String, sqlx::Error> {
    row.try_get("id")
}

/// The relay's configuration, as the object the stream carries. An array, a
/// scalar or `null` is refused: the wire type is a map, and a row the announcing
/// delta cannot describe is a row the stream cannot represent.
fn parse_relay_config(raw: &str) -> Result<Map<String, Value>, ConnectError> {
    serde_json::from_str(raw).map_err(|error| invalid(format!("invalid configJson: {error}")))
}

/// The published payload, opaque by contract: any JSON value is a payload.
fn parse_payload(raw: &str) -> Result<Value, ConnectError> {
    serde_json::from_str(raw).map_err(|error| invalid(format!("invalid payloadJson: {error}")))
}

/// The two relay kinds the wire defines, or `InvalidArgument` naming what
/// arrived.
fn parse_relay_kind(raw: &str) -> Result<McpRelayKind, ConnectError> {
    match raw {
        "stdio" => Ok(McpRelayKind::Stdio),
        "sse" => Ok(McpRelayKind::Sse),
        other => Err(invalid(format!("invalid relay kind {other:?}"))),
    }
}

/// A relay id from a request, as the branded id every statement binds. v2 only
/// reached this check after resolving the row, where a malformed id was a 500;
/// here it is the caller's own argument, so it is `InvalidArgument`.
fn parse_relay_id(raw: &str) -> Result<McpRelayId, ConnectError> {
    McpRelayId::try_from(raw)
        .map_err(|error| invalid(format!("invalid relay id {raw:?}: {error}")))
}

/// A fresh relay id: 16 bytes of CSPRNG entropy, version and variant set so the
/// value is the shape `crypto.randomUUID()` produced in v2.
///
/// `/dev/urandom` is the entropy source `push::vapid::P256KeypairGenerator`
/// already uses, for the same reason (Linux and macOS only, both ship the
/// device). It is a second spelling of one facility and the hoist that removes it
/// is the lead's edit; it lives here because this is the only minted id in the
/// sessions domain today.
fn mint_relay_id() -> Result<McpRelayId, ConnectError> {
    let mut bytes = [0_u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(&mut bytes))
        .map_err(|error| internal(format!("mcp create: no entropy source: {error}")))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    // The 8-4-4-4-12 grouping, cut from the rendered hex rather than assembled
    // byte by byte.
    let rendered = hex::encode(bytes);
    let groups = [0, 8, 12, 16, 20, 32];
    let id = groups
        .windows(2)
        .map(|ends| &rendered[ends[0]..ends[1]])
        .collect::<Vec<_>>()
        .join("-");
    parse_relay_id(&id)
}

/// The one refusal for a relay this coordinator cannot resolve, for the delete
/// and the publish alike. The dashboard scope is part of what it says: an id
/// this coordinator does not hold answers exactly as one that never existed.
fn relay_not_found() -> ConnectError {
    ConnectError::new(ErrorCode::NotFound, "not found")
}

/// Refuse anything that is not a browser, with the marker header a client needs
/// to tell "log in again" from "this method needs a device credential"
/// (`auth-interceptor.ts:265-271`).
fn require_account_device(caller: &Caller) -> Result<&str, ConnectError> {
    caller.principal.require_account_device().map_err(|_| {
        let mut error = ConnectError::new(ErrorCode::Unauthenticated, "authentication required");
        error.response_headers_mut().insert(
            axum::http::HeaderName::from_static(crate::auth::principal::AUTH_LAYER_HEADER),
            axum::http::HeaderValue::from_static(crate::auth::principal::AUTH_LAYER_DEVICE),
        );
        error
    })
}
