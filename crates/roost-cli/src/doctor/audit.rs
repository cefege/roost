//! The audit-log half of `roost doctor`: what the COORDINATOR already recorded
//! about the requests it refused or failed, read straight out of `audit_log`.
//! Called by doctor/mod.rs; its rendered section is spliced into the digest by
//! doctor/digest_render.rs.
//!
//! This reports, it does not diagnose. Every row is the coordinator's own
//! verdict — the method it routed, the status it returned, the caller it
//! attributed the call to — and the section's whole job is to put those verdicts
//! in front of an operator beside the log signals. Re-deriving "why did this
//! fail" here would be a second opinion about a decision someone else already
//! made and recorded, and `docs/FAILURE-INDEX.md` exists precisely because
//! diagnosis belongs where the evidence was.

use std::path::{Path, PathBuf};

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow};
use sqlx::{Row, SqlitePool};

use crate::utc_clock::format_utc_minute;

/// The first status that counts as the coordinator's own failure rather than a
/// caller's. Everything below it is a decision the coordinator made on purpose
/// — an unauthenticated probe, a revoked key, a malformed request — and an
/// operator causes those daily, including by running `roost status` itself.
pub const SERVER_ERROR_STATUS: i64 = 500;

/// How many audit rows the digest shows. A window with a thousand rejected
/// probes has one thing wrong with it — a client retrying — and printing a
/// thousand rows buries the three 500s underneath them.
pub const AUDIT_ROWS_SHOWN: usize = 15;

/// One `(status, method, path)` triple and how often the coordinator returned it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditFailureGroup {
    pub status: i64,
    pub method: String,
    pub path: String,
    pub count: u64,
    pub latest_ms: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditSummary {
    pub groups: Vec<AuditFailureGroup>,
    /// Rows at or above [`SERVER_ERROR_STATUS`], counted individually rather
    /// than per group because the exit decision is about the coordinator
    /// failing at all, not about which method it was.
    pub server_error_count: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    #[error("coordinator database not found: {0}")]
    Missing(PathBuf),
    #[error("coordinator database unreadable: {0}")]
    Unreadable(String),
}

/// Every refused or failed call in the window, grouped. A database with no
/// `audit_log` yet is an empty summary, not an error: a coordinator that has
/// never served a request has nothing to review, and refusing the command would
/// make a fresh install undiagnosable.
pub async fn read_audit_failures(
    database_path: &Path,
    cutoff_ms: i64,
) -> Result<AuditSummary, AuditError> {
    if !database_path.exists() {
        return Err(AuditError::Missing(database_path.to_path_buf()));
    }
    let options = SqliteConnectOptions::new()
        .filename(database_path)
        .read_only(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .map_err(unreadable)?;
    let summary = read_failures(&pool, cutoff_ms).await;
    pool.close().await;
    summary
}

async fn read_failures(pool: &SqlitePool, cutoff_ms: i64) -> Result<AuditSummary, AuditError> {
    if !table_exists(pool).await {
        return Ok(AuditSummary::default());
    }
    let rows = sqlx::query(
        "SELECT method, path, status, ts FROM audit_log \
         WHERE ts >= ?1 AND status >= 400 ORDER BY ts",
    )
    .bind(cutoff_ms)
    .fetch_all(pool)
    .await
    .map_err(unreadable)?;
    Ok(fold(rows))
}

async fn table_exists(pool: &SqlitePool) -> bool {
    sqlx::query(
        "SELECT 1 AS present FROM sqlite_master \
         WHERE type = 'table' AND name = 'audit_log' LIMIT 1",
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .is_some()
}

fn fold(rows: Vec<SqliteRow>) -> AuditSummary {
    let mut groups: Vec<AuditFailureGroup> = Vec::new();
    let mut server_error_count = 0;
    for row in rows {
        let status: i64 = row.try_get("status").unwrap_or(0);
        let method: String = row.try_get("method").unwrap_or_default();
        let path: String = row.try_get("path").unwrap_or_default();
        let ts: i64 = row.try_get("ts").unwrap_or(0);
        if status >= SERVER_ERROR_STATUS {
            server_error_count += 1;
        }
        let existing = groups
            .iter_mut()
            .find(|group| group.status == status && group.method == method && group.path == path);
        match existing {
            Some(group) => {
                group.count += 1;
                group.latest_ms = group.latest_ms.max(ts);
            }
            None => groups.push(AuditFailureGroup {
                status,
                method,
                path,
                count: 1,
                latest_ms: ts,
            }),
        }
    }
    // Busiest first, then lowest status, then by name: a window reviewed twice
    // must print the same order both times.
    groups.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then(left.status.cmp(&right.status))
            .then(left.method.cmp(&right.method))
            .then(left.path.cmp(&right.path))
    });
    AuditSummary {
        groups,
        server_error_count,
    }
}

/// The digest's audit section, as lines. A 4xx-only window prints its rows and
/// still exits 0; that is the whole reason this section can sit next to a gate
/// without making the gate permanently red.
pub fn render_section(summary: &AuditSummary) -> Vec<String> {
    let mut lines = vec!["## audit (coordinator request log)".to_string()];
    if summary.groups.is_empty() {
        lines.push("  ✓ no failed calls in window".to_string());
        return lines;
    }
    for group in summary.groups.iter().take(AUDIT_ROWS_SHOWN) {
        lines.push(format!(
            "  {:>4}  {} {} {}  last {}",
            group.count,
            group.status,
            group.method,
            group.path,
            format_utc_minute(group.latest_ms)
        ));
    }
    lines
}

fn unreadable(error: sqlx::Error) -> AuditError {
    AuditError::Unreadable(error.to_string())
}
