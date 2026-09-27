//! The `mcp_relays` table as the MCP domain sees it: the four statements, the
//! row type, and the deadline every one of them runs under.
//!
//! Owned by `sessions::mcp`, which is the only caller. Split out of it so the
//! question "what does a call wait on, and for how long" is answered in one
//! place rather than in the middle of a Connect handler.
//!
//! Every statement binds its values. None interpolates a caller's string, and
//! none of them can name a relay this coordinator's dashboard does not hold:
//! that scope is in the `WHERE` of all four, which is the whole boundary this
//! surface has (see `sessions::mcp::handle_mcp_publish`).

use std::time::Duration;

use connectrpc::{ConnectError, ErrorCode};
use roost_proto as proto;
use sqlx::Row as _;

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
pub const STORE_DEADLINE: Duration = crate::db::BUSY_TIMEOUT;

/// Two statuses, because each tells the browser something different:
/// `Unavailable` means "the coordinator's own state did not answer; come back",
/// which a browser retries, and `Internal` means a statement failed, which it
/// does not. One status would either send a browser into a retry loop against a
/// broken database or make a retryable wait look permanent.
pub async fn within_store_deadline<T>(
    method: &'static str,
    work: impl Future<Output = Result<T, sqlx::Error>>,
) -> Result<T, ConnectError> {
    match tokio::time::timeout(STORE_DEADLINE, work).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(refusal_for(method, error)),
        Err(_elapsed) => {
            tracing::error!(
                method,
                deadline_ms = STORE_DEADLINE.as_millis(),
                "mcp: the store did not answer inside the busy timeout; refusing rather than holding the caller"
            );
            Err(did_not_answer(method))
        }
    }
}

/// What a failed statement means to a caller.
///
/// READ THIS BEFORE GENERALISING IT. `PoolTimedOut` is a condition v3
/// INVENTED and v2 has no analogue for: v2's store is one connection with no
/// pool, so it can be genuinely busy but can never fail to hand one out. The
/// parity rule therefore has no v2 answer to select here, and the answer is
/// this domain's own.
///
/// This domain answers `Unavailable` because v2's publish path establishes it:
/// a relay publish that cannot be stored is a call the browser retries, and
/// `handlers-mcp.ts` never wrote a row a caller could be told to fix.
///
/// **FIVE SITES MUST STAY `Internal`,** and this is the list that stops the
/// "fix": `push/rpc.rs`, `ui_state/fence.rs`, `workers/rpc.rs`,
/// `sessions/tasks.rs` and `diagnostics/rpc_transcription.rs` all map a store
/// error to `Internal` with a blanket `.to_string()`. Their domains DO have a
/// v2 answer, and it is `Internal` — v2 reports a busy database that way — so
/// propagating this mapping would be a parity regression on five methods in
/// order to make one shape.
///
/// A shared helper for store errors is still the right destination, but it has
/// to take the domain's DECLARED mapping as an argument rather than hard-code
/// one. A helper that hard-codes `Unavailable` is this same bug one level up.
///
/// Within this domain the rest is mechanical: a `PoolTimedOut` IS the deadline
/// arm, reached a few microseconds earlier by the pool's own timer, because the
/// coordinator keeps one connection and both timers are armed from the same
/// [`STORE_DEADLINE`]. Answering it `Internal` would report "your statement
/// failed" for a statement that never started, which is the one thing the
/// status cannot mean — a browser does not retry those. Every other error is a
/// real statement failure and stays `Internal`.
fn refusal_for(method: &'static str, error: sqlx::Error) -> ConnectError {
    if matches!(error, sqlx::Error::PoolTimedOut) {
        tracing::error!(
            method,
            deadline_ms = STORE_DEADLINE.as_millis(),
            "mcp: the store's single connection was not free inside the busy timeout"
        );
        return did_not_answer(method);
    }
    ConnectError::new(ErrorCode::Internal, error.to_string())
}

/// The one refusal for "the coordinator's own state did not answer", named for
/// the method that was waiting so a reader of the browser console knows which
/// call to retry.
fn did_not_answer(method: &'static str) -> ConnectError {
    ConnectError::new(
        ErrorCode::Unavailable,
        format!(
            "the coordinator store did not answer {method} within {}ms",
            STORE_DEADLINE.as_millis()
        ),
    )
}

/// The caller's own mistake — the one status worth separating from the
/// coordinator's, because the browser fixes it rather than retrying it.
///
/// It lives beside [`internal`] because the two are one decision: every refusal
/// in this domain is either the caller's to fix or the deployment's, and a
/// third status would only blur which.
pub fn invalid(reason: String) -> ConnectError {
    ConnectError::new(ErrorCode::InvalidArgument, reason)
}

/// A coordinator that is misassembled or whose store failed: both the
/// deployment's fault, neither the caller's to retry.
pub fn internal(reason: String) -> ConnectError {
    ConnectError::new(ErrorCode::Internal, reason)
}

/// One `mcp_relays` row, exactly as the column holds it.
#[derive(Debug, sqlx::FromRow)]
pub struct StoredRelay {
    /// The relay's minted id, a UUID.
    pub id: String,
    /// The operator's name for it.
    pub label: String,
    /// `stdio` or `sse`, as written.
    pub kind: String,
    /// The caller's config bytes, unparsed.
    pub config_json: String,
    /// When the row was written, in epoch milliseconds.
    pub created_at_ms: i64,
}

impl StoredRelay {
    /// The wire relay a row answers with, from the column's bytes unparsed: v2
    /// never looks at the config, and a registry that can fail to list because
    /// one hand-edited row's JSON is malformed is not a registry.
    pub fn into_proto(self) -> Result<proto::McpRelay, ConnectError> {
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

/// The relay's rows, oldest first.
///
/// The order is stated because v2 left it to SQLite, which answers a bare
/// `SELECT` in whatever order storage hands back. The pane sorts client-side so
/// no consumer depended on it, but a stable answer is the difference between a
/// caller that can rely on it and one that cannot.
pub const LIST_RELAYS: &str = "SELECT id, label, kind, config_json, created_at_ms FROM mcp_relays \
                              WHERE dashboard_id = ?1 ORDER BY created_at_ms, id";
pub const INSERT_RELAY: &str = "INSERT INTO mcp_relays \
                               (id, label, kind, config_json, created_at_ms, dashboard_id) \
                               VALUES (?1, ?2, ?3, ?4, ?5, ?6)";

/// The delete and the existence decision in one statement.
///
/// A select then a delete is two decisions with a window between them, and the
/// two-statement form also answers about a relay that stopped existing in
/// between.
pub const DELETE_RELAY: &str =
    "DELETE FROM mcp_relays WHERE id = ?1 AND dashboard_id = ?2 RETURNING id";

/// Whether this dashboard holds the relay, which is the authorization for
/// publishing against it.
pub const RELAY_HELD: &str = "SELECT id FROM mcp_relays WHERE id = ?1 AND dashboard_id = ?2";

/// The `RETURNING id` column of a removed relay, as text.
///
/// The statement bound the id, so what comes back is what was bound; branding it
/// is the caller's job, and a row answering with anything else is reported
/// rather than papered over.
pub fn removed_id(row: sqlx::sqlite::SqliteRow) -> Result<String, sqlx::Error> {
    row.try_get("id")
}
