//! `CoordinatorService.AuditList` — the operator's window onto `audit_log`.
//!
//! Ported from `handlers-system.ts:311-337`. The handler's shape is a keyset
//! page, not an offset page: rows come back newest-first and the cursor is the
//! last row's own id, so a page boundary is a value rather than a count and
//! rows inserted while an operator is reading cannot shift the window under
//! them.
//!
//! ONE EXTRA ROW IS FETCHED AND DROPPED. `has_more` is "there was a row past
//! the limit", which is the only way to know a next page exists without a
//! second COUNT over a table that reaches fifty million rows — and it is why
//! the limit the statement binds is `limit + 1`.
//!
//! THE CURSOR IS NOT VALIDATED, AND THAT IS v2's ANSWER. `parseInt` on a
//! non-numeric cursor yields `NaN`, `id < NaN` is false in SQLite, and the page
//! comes back empty rather than refused. The only writer of this cursor is this
//! method — it is `String(lastRow.id)` — so a caller that sends anything else
//! has a bug in its own paging loop, and an empty page stops that loop where an
//! error would only be logged and retried.

use connectrpc::{ConnectError, ErrorCode, ServiceResult};
use roost_proto as proto;
use sqlx::SqlitePool;

use crate::auth::principal::require_account_device;
use crate::coord_core::{Caller, CoordCore};
use crate::rpc::service::ok_response;

/// The page a caller gets when it names no size. v2's `req.limit || 100`.
const DEFAULT_LIMIT: u32 = 100;

/// The largest page a caller may ask for. v2's `Math.min(…, 500)`: a page is
/// for a human reading a settings pane, not for exporting the table.
const MAX_LIMIT: u32 = 500;

/// `CoordinatorService.AuditList`.
pub async fn handle_audit_list(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AuditListRequest,
) -> ServiceResult<proto::AuditListResponse> {
    require_account_device(caller)?;
    let limit = request
        .limit
        .filter(|asked| *asked != 0)
        .unwrap_or(DEFAULT_LIMIT)
        .min(MAX_LIMIT);
    let mut rows = read_page(core.services.db.pool(), &request, limit)
        .await
        .map_err(|error| ConnectError::new(ErrorCode::Internal, error.to_string()))?;

    // The extra row exists only to answer this question, and answering it by
    // reading one row further is why the statement above binds `limit + 1`.
    let has_more = rows.len() > limit as usize;
    rows.truncate(limit as usize);
    let next_cursor = has_more
        .then(|| rows.last().map(|row| row.id.to_string()))
        .flatten();
    ok_response(proto::AuditListResponse {
        rows: rows.into_iter().map(AuditPageRow::into_proto).collect(),
        next_cursor,
        ..Default::default()
    })
}

/// One page of rows, newest first, as the statement returned them.
#[derive(Debug, sqlx::FromRow)]
struct AuditPageRow {
    id: i64,
    ts: i64,
    caller_fp: Option<String>,
    caller_label: Option<String>,
    method: String,
    path: String,
    status: i64,
    trace_id: Option<String>,
}

impl AuditPageRow {
    /// The wire row. A NULL column is an absent field rather than an empty
    /// string, because the pane distinguishes "no caller" from "a caller with
    /// no name" and a defaulted proto3 string is indistinguishable from the
    /// former.
    fn into_proto(self) -> proto::AuditRow {
        proto::AuditRow {
            id: self.id.unsigned_abs(),
            ts: self.ts.unsigned_abs(),
            caller_fp: self.caller_fp,
            caller_label: self.caller_label,
            method: self.method,
            path: self.path,
            status: self.status.unsigned_abs() as u32,
            trace_id: self.trace_id,
            ..Default::default()
        }
    }
}

/// The page statement, with the caller's filters bound rather than interpolated.
async fn read_page(
    pool: &SqlitePool,
    request: &proto::AuditListRequest,
    limit: u32,
) -> Result<Vec<AuditPageRow>, sqlx::Error> {
    let statement = sqlx::query_as::<_, AuditPageRow>(
        "SELECT a.id AS id, a.ts AS ts, a.caller_fp AS caller_fp, \
         k.label AS caller_label, a.method AS method, a.path AS path, \
         a.status AS status, a.trace_id AS trace_id \
         FROM audit_log a LEFT JOIN authorized_keys k ON k.fingerprint = a.caller_fp \
         WHERE (?1 IS NULL OR a.id < ?1) \
         AND (?2 IS NULL OR a.caller_fp = ?2) \
         AND (?3 IS NULL OR a.method = ?3) \
         ORDER BY a.id DESC LIMIT ?4",
    )
    .bind(request.cursor.as_deref().and_then(|value| value.parse::<i64>().ok()))
    .bind(request.caller_fp.as_deref())
    .bind(request.method.as_deref())
    .bind(i64::from(limit) + 1);
    statement.fetch_all(pool).await
}
