//! The validation every UI method shares before it touches state: bounded
//! caller text, a persisted row for every session it names, and the operator
//! label of each reporting key.
//!
//! None of the four `ui_state` methods is fenced to the caller's tab: v2's
//! `handlers-ui.ts` calls only `requireAccountDevice`, and the tab a method acts
//! on is the one the request BODY names. A request naming a session with no
//! `sessions` row would put a command on the bus for a pane the fleet has never
//! heard of, so that half is refused here.

use std::collections::HashMap;

use connectrpc::{ConnectError, ErrorCode};
use roost_protocol::validate::max_utf8_bytes;
use sqlx::{AnyPool, Row};

use crate::db::{IN_LIST_CHUNK, SqlBuilder, push_in_list};

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
/// The lookup runs over the DISTINCT ids, one statement per
/// [`IN_LIST_CHUNK`], and a miss is `NotFound` rather than `InvalidArgument`:
/// the request was well formed, the session it names is simply not one this
/// coordinator has. Every id is a bound value, so no caller's text ever reaches
/// the SQL.
pub async fn require_persisted_sessions(
    pool: &AnyPool,
    session_ids: &[String],
) -> Result<(), ConnectError> {
    let mut distinct: Vec<&str> = session_ids.iter().map(String::as_str).collect();
    distinct.sort_unstable();
    distinct.dedup();
    let mut found: Vec<String> = Vec::with_capacity(distinct.len());
    for chunk in distinct.chunks(IN_LIST_CHUNK) {
        let mut statement = SqlBuilder::new("SELECT id FROM sessions WHERE id IN ");
        push_in_list(&mut statement, chunk);
        let rows = statement
            .build()
            .fetch_all(pool)
            .await
            .map_err(|error| ConnectError::new(ErrorCode::Internal, error.to_string()))?;
        found.extend(rows.iter().map(|row| row.get::<String, _>("id")));
    }
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
    pool: &AnyPool,
    fingerprints: &[String],
) -> Result<HashMap<String, String>, ConnectError> {
    let mut distinct: Vec<&str> = fingerprints.iter().map(String::as_str).collect();
    distinct.sort_unstable();
    distinct.dedup();
    let mut labels = HashMap::new();
    for chunk in distinct.chunks(IN_LIST_CHUNK) {
        let mut statement =
            SqlBuilder::new("SELECT fingerprint, label FROM authorized_keys WHERE fingerprint IN ");
        push_in_list(&mut statement, chunk);
        let rows = statement
            .build()
            .fetch_all(pool)
            .await
            .map_err(|error| ConnectError::new(ErrorCode::Internal, error.to_string()))?;
        for row in rows {
            labels.insert(
                row.get::<String, _>("fingerprint"),
                row.get::<String, _>("label"),
            );
        }
    }
    Ok(labels)
}
