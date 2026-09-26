//! The two facts every UI method requires before it touches state: a browser
//! tab the request is fenced to, and a persisted row for every session it
//! names.
//!
//! All four `ui_state` methods are `DevicePlusFence`
//! (`rpc::method_route::ROWS_UI_STATE`), and each half is load-bearing. A
//! request without a tab id has no tab to answer to, so a layout apply aimed at
//! a tab could not be attributed to the socket that proved it; a request naming
//! a session with no `sessions` row would put a command on the bus for a pane
//! the fleet has never heard of.

use std::collections::HashMap;

use connectrpc::{ConnectError, ErrorCode};
use roost_protocol::validate::max_utf8_bytes;
use sqlx::Row;
use sqlx::sqlite::SqlitePool;

use crate::coord_core::Caller;

/// The tab this request is fenced to, or the refusal that names the fence.
pub fn require_tab_fence<'caller>(
    caller: &'caller Caller,
    method: &str,
) -> Result<&'caller str, ConnectError> {
    match caller.tab_id.as_deref() {
        Some(tab_id) if !tab_id.is_empty() => Ok(tab_id),
        _ => Err(ConnectError::new(
            ErrorCode::FailedPrecondition,
            format!(
                "{method} requires the browser tab fence: this request carried no tab id \
                 (x-roost-tab-id), so it cannot be attributed to one live tab socket"
            ),
        )),
    }
}

/// A bounded UI text field, optionally required to be non-blank.
pub fn require_bounded_ui_text(
    value: &str,
    max_bytes: usize,
    field: &str,
    required: bool,
) -> Result<(), ConnectError> {
    if required && value.trim().is_empty() {
        return Err(ConnectError::new(
            ErrorCode::InvalidArgument,
            format!("invalid {field}"),
        ));
    }
    max_utf8_bytes(field, value, max_bytes)
        .map_err(|_| ConnectError::new(ErrorCode::InvalidArgument, format!("invalid {field}")))
}

/// Refuse a request that names a session with no `sessions` row.
///
/// The lookup is one statement over the DISTINCT ids, and a miss is `NotFound`
/// rather than `InvalidArgument`: the request was well formed, the session it
/// names is simply not one this coordinator has.
///
/// The statement text is built, and what is interpolated into it is a run of `?`
/// placeholders whose length is the deduplicated id count -- no caller's text
/// ever reaches the SQL. SQLite's variable limit is 32766 by default and the
/// caller's own document bounds put a command far below it.
pub async fn require_persisted_sessions(
    pool: &SqlitePool,
    session_ids: &[String],
) -> Result<(), ConnectError> {
    let mut distinct: Vec<&str> = session_ids.iter().map(String::as_str).collect();
    distinct.sort_unstable();
    distinct.dedup();
    if distinct.is_empty() {
        return Ok(());
    }
    let query = format!(
        "SELECT id FROM sessions WHERE id IN ({})",
        vec!["?"; distinct.len()].join(", ")
    );
    let mut statement = sqlx::query(sqlx::AssertSqlSafe(query));
    for session_id in &distinct {
        statement = statement.bind(session_id);
    }
    let rows = statement
        .fetch_all(pool)
        .await
        .map_err(|error| ConnectError::new(ErrorCode::Internal, error.to_string()))?;
    let found: Vec<String> = rows.iter().map(|row| row.get::<String, _>("id")).collect();
    if distinct
        .iter()
        .any(|session_id| !found.iter().any(|held| held == session_id))
    {
        return Err(ConnectError::new(ErrorCode::NotFound, "session not found"));
    }
    Ok(())
}

/// The operator-facing label for each fingerprint that still has a key row.
///
/// One query for every distinct fingerprint in a list, and an absent row is an
/// empty label rather than a missing entry: a key can be revoked while its tab
/// is still reporting, and that tab must still be listed.
pub async fn labels_for_fingerprints(
    pool: &SqlitePool,
    fingerprints: &[String],
) -> Result<HashMap<String, String>, ConnectError> {
    let mut distinct: Vec<&str> = fingerprints.iter().map(String::as_str).collect();
    distinct.sort_unstable();
    distinct.dedup();
    let mut labels = HashMap::new();
    if distinct.is_empty() {
        return Ok(labels);
    }
    let query = format!(
        "SELECT fingerprint, label FROM authorized_keys WHERE fingerprint IN ({})",
        vec!["?"; distinct.len()].join(", ")
    );
    let mut statement = sqlx::query(sqlx::AssertSqlSafe(query));
    for fingerprint in &distinct {
        statement = statement.bind(fingerprint);
    }
    let rows = statement
        .fetch_all(pool)
        .await
        .map_err(|error| ConnectError::new(ErrorCode::Internal, error.to_string()))?;
    for row in rows {
        labels.insert(
            row.get::<String, _>("fingerprint"),
            row.get::<String, _>("label"),
        );
    }
    Ok(labels)
}
