//! The per-request audit hook: the only place an `audit_log` row is written and
//! the only place one is published to `audit_bus`. Owned by the audit slice; the
//! middleware layer mounts it and calls [`record_request`] once per request on
//! the way out. Depends on `events`, `db` and the `write_gate` audit policy.
//!
//! **THE HOOK CANNOT FAIL THE REQUEST IT AUDITS.** A request that succeeded and
//! then returned 500 because its own audit row could not be written has traded
//! an audit outage for an availability outage, and the operator now has neither.
//! A failed write is reported in the log and in the returned value; the response
//! is already decided by the time the hook runs.
//!
//! **A REFUSED REQUEST IS AUDITED, NOT ONLY A SUCCEEDED ONE.** A log that
//! records only successes cannot answer "who tried this", which is the one
//! question it exists for. The skips below are a list of high-volume noise, not
//! a success filter.
//!
//! **ONE ROW PER REQUEST.** The RPC interceptor and the outer HTTP layer both
//! see the same request, and v2 shipped an incident for it: the outer wrapper
//! wrote rows for requests the auth interceptor had already authenticated, so
//! every `caller_fp` was NULL (`docs/FAILURE-INDEX.md`, "audit_log caller_fp is
//! NULL for every authed RPC"). A record is consumed by its first call whatever
//! the outcome, so a second layer writing the same request writes nothing.

use connectrpc::{ErrorCode, RequestContext};
use roost_observability::LogFields;
use roost_observability::log::error as log_error;
use sqlx::{QueryBuilder, Row, Sqlite};

use crate::coord_core::{Caller, CoordCore, ListenerTrust};
use crate::db::CoordDb;
use crate::events::bus_messages::AuditRow;
use crate::write_gate::{method_never_persists_audit, should_persist_method_audit};

/// The log target the audit write path reports under.
const AUDIT_TARGET: &str = "middleware.audit";

/// The event name an operator greps when the audit log has a hole in it.
const WRITE_FAILED: &str = "audit.write_failed";

/// The event name for a row written with no tenancy scope.
const SCOPE_MISSING: &str = "audit.scope_missing";

/// The HTTP verb a Connect row records.
///
/// Every Connect RPC is a POST, and the retention janitor keys
/// `method IN ('GET','HEAD')` to find anonymous static reads
/// (`maintenance::audit_retention::cleanup_anonymous_static_audit_log`), so the
/// column is the transport's verb rather than the procedure's name.
const CONNECT_HTTP_METHOD: &str = "POST";

/// The non-Connect surfaces the listener serves.
///
/// Connect is deliberately not a variant: only [`AuditRecord::connect`] builds
/// a Connect row, so the outer layer cannot write one for a request whose
/// credential it never resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NonConnectSurface {
    /// A static asset or a deep-link document.
    Spa,
    /// The database export.
    DbExport,
    /// An `/api/*` path that no route claimed.
    Api,
}

/// Which predicate decides a record, and what it needs to decide it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Surface {
    /// A Connect RPC, with the proto procedure name the policy keys on and the
    /// listener's answer to "whose address is this really".
    Connect {
        procedure: String,
        listener: ListenerTrust,
    },
    /// A plain HTTP request the listener answered itself.
    NonConnect(NonConnectSurface),
}

/// Whether a non-Connect outcome is worth a durable row.
///
/// "an unmatched /api/* path is an unauthenticated GET the rate limiter lets
/// through, so one durable row per probed path is pure amplification" -- and the
/// janitor that would delete those rows only sweeps anonymous *static* reads, so
/// an amplified row is not even self-cleaning (`security.ts:114`).
#[must_use]
pub fn should_persist_non_connect_audit(
    surface: NonConnectSurface,
    method: &str,
    status: u16,
) -> bool {
    if surface == NonConnectSurface::Api && status == 404 {
        return false;
    }
    let read = method == "GET" || method == "HEAD";
    !(surface == NonConnectSurface::Spa && read && (200..400).contains(&status))
}

/// Whether a Connect outcome on a named listener is worth a durable row.
///
/// The one skip: an anonymous credential failure through the operator's front
/// door. It names no identity, and `audit_log` has no address column, so it is
/// unbounded volume with no forensic value -- and the retention sweep is an
/// explicit allowlist that never ages out auth rows, so those rows are permanent.
/// That table once reached 7,026,358 rows / 1.0 GB (`docs/FAILURE-INDEX.md`).
/// The same 401 on a directly-observed listener persists: that caller is low
/// volume and names a host, not a stranger.
#[must_use]
pub fn should_persist_connect_audit(
    listener: ListenerTrust,
    status: u16,
    caller_fp: Option<&str>,
) -> bool {
    !(listener == ListenerTrust::Forwarded && status == 401 && caller_fp.is_none())
}

/// The HTTP status `audit_log` records for a Connect code.
///
/// Dashboards read `WHERE status >= 400`, so the row carries HTTP semantics
/// rather than the code's own name (`auth-interceptor.ts:62-78`).
#[must_use]
pub fn connect_status(code: ErrorCode) -> u16 {
    match code {
        ErrorCode::InvalidArgument | ErrorCode::OutOfRange => 400,
        ErrorCode::Unauthenticated => 401,
        ErrorCode::PermissionDenied => 403,
        ErrorCode::NotFound => 404,
        ErrorCode::AlreadyExists | ErrorCode::Aborted => 409,
        ErrorCode::FailedPrecondition => 412,
        ErrorCode::ResourceExhausted => 429,
        ErrorCode::Unimplemented => 501,
        ErrorCode::Unavailable => 503,
        ErrorCode::DeadlineExceeded => 504,
        ErrorCode::Canceled | ErrorCode::Unknown | ErrorCode::Internal | ErrorCode::DataLoss => 500,
    }
}

/// The fingerprint of the caller the auth gate stamped on a request, or `None`
/// for a request it refused -- which is precisely the row that answers "who
/// tried this", so an absent caller is never a reason to stay silent.
#[must_use]
pub fn caller_fingerprint(context: &RequestContext) -> Option<String> {
    context
        .extensions()
        .get::<Caller>()
        .map(|caller| caller.fingerprint().to_owned())
}

/// One request's audit row, assembled by the mounting layer.
///
/// Every field is supplied by the caller rather than re-read from the request:
/// the layer that authenticated it is the only one that can say who the caller
/// was, and a second reader of that credential is a second answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRecord {
    method: String,
    path: String,
    status: u16,
    caller_fp: Option<String>,
    trace_id: Option<String>,
    pair_confirmation_failed: bool,
    surface: Surface,
    written: bool,
}

impl AuditRecord {
    /// A Connect RPC's outcome.
    ///
    /// `service` and `procedure` are the proto names from the procedure spec,
    /// never the URL: the row's `path` is built here as `/{service}/{procedure}`,
    /// the spelling the retention allowlist and every dashboard query key on.
    #[must_use]
    pub fn connect(
        service: &str,
        procedure: &str,
        status: u16,
        caller_fp: Option<String>,
        trace_id: Option<String>,
        listener: ListenerTrust,
    ) -> Self {
        Self {
            method: CONNECT_HTTP_METHOD.to_string(),
            path: format!("/{service}/{procedure}"),
            status,
            caller_fp,
            trace_id,
            pair_confirmation_failed: false,
            surface: Surface::Connect {
                procedure: procedure.to_string(),
                listener,
            },
            written: false,
        }
    }

    /// A plain HTTP request the listener answered itself.
    #[must_use]
    pub fn non_connect(
        surface: NonConnectSurface,
        method: &str,
        path: &str,
        status: u16,
        trace_id: Option<String>,
    ) -> Self {
        Self {
            method: method.to_string(),
            path: path.to_string(),
            status,
            caller_fp: None,
            trace_id,
            pair_confirmation_failed: false,
            surface: Surface::NonConnect(surface),
            written: false,
        }
    }

    /// Record that a `PairConfirm` returned `ok: false`. A *successful*
    /// `PairConfirm` is on the skip list; a refused one is the row an operator
    /// needs ("who tried to authorize which device, and failed").
    #[must_use]
    pub fn pair_confirmation_failed(mut self, failed: bool) -> Self {
        self.pair_confirmation_failed = failed;
        self
    }

    fn skip_reason(&self) -> Option<AuditSkip> {
        match &self.surface {
            Surface::Connect {
                procedure,
                listener,
            } => {
                if method_never_persists_audit(procedure) {
                    Some(AuditSkip::NeverPersists)
                } else if !should_persist_method_audit(
                    procedure,
                    self.status,
                    self.pair_confirmation_failed,
                ) {
                    Some(AuditSkip::SuccessWithoutSignal)
                } else if !should_persist_connect_audit(
                    *listener,
                    self.status,
                    self.caller_fp.as_deref(),
                ) {
                    Some(AuditSkip::AnonymousFrontDoorRefusal)
                } else {
                    None
                }
            }
            Surface::NonConnect(surface) => (!should_persist_non_connect_audit(
                *surface,
                &self.method,
                self.status,
            ))
            .then_some(AuditSkip::LowValueHttpRead),
        }
    }
}

/// Why an outcome is not worth a row, when it is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditSkip {
    /// This method writes no row whatever the outcome.
    NeverPersists,
    /// A successful method whose row would carry no forensic signal.
    SuccessWithoutSignal,
    /// An anonymous refusal that arrived through the operator's front door.
    AnonymousFrontDoorRefusal,
    /// A static or deep-link read, or an unmatched `/api/*` path.
    LowValueHttpRead,
    /// This record already produced its one row.
    AlreadyRecorded,
}

/// What the hook did with a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditOutcome {
    /// The row is durable; this is its id.
    Written { id: i64 },
    /// No row, and why not.
    Skipped(AuditSkip),
    /// The write failed. The request's own outcome is untouched.
    WriteFailed { error: String },
}

/// Write this request's row, if it deserves one, and report what happened.
///
/// The entry point the middleware layer mounts. It is total by construction:
/// there is no error for a caller to propagate into a response, because a
/// response this runs beside has already been decided.
pub async fn record_request(core: &CoordCore, record: &mut AuditRecord) -> AuditOutcome {
    if record.written {
        return AuditOutcome::Skipped(AuditSkip::AlreadyRecorded);
    }
    // Consumed whatever happens next, including a failed write: a retry would
    // log the same request twice, which is the duplicate this prevents.
    record.written = true;
    if let Some(reason) = record.skip_reason() {
        return AuditOutcome::Skipped(reason);
    }
    let path = record.path.clone();
    match write_audit_rows(core, std::slice::from_ref(record)).await {
        Ok(committed) => match committed.first() {
            Some(row) => AuditOutcome::Written { id: row.id },
            None => AuditOutcome::WriteFailed {
                error: "the audit insert committed no row".to_string(),
            },
        },
        Err(error) => {
            let fields = LogFields::new()
                .set("error", error.to_string())
                .set("path", path);
            log_error(AUDIT_TARGET, WRITE_FAILED, fields);
            AuditOutcome::WriteFailed {
                error: error.to_string(),
            }
        }
    }
}

/// Write these records as one atomic batch, then publish the committed rows to
/// `audit_bus` in durable id order.
///
/// The batch is a single multi-row INSERT rather than a transaction around N
/// statements: one SQLite statement is already atomic, so BEGIN/COMMIT would be
/// two round trips on the single pooled connection every audited request shares,
/// bought for nothing. The publish happens after the commit, so no subscriber
/// sees a row that is not durable, and the order is by committed id, so a live
/// stream agrees with what a later read of the table returns.
///
/// This is the primitive for a caller that must know whether the row landed
/// (terminal input, `apps/coord/src/terminal/input/input-control.ts:117`). Every
/// other caller uses [`record_request`], which cannot fail.
pub async fn write_audit_rows(
    core: &CoordCore,
    records: &[AuditRecord],
) -> Result<Vec<AuditRow>, sqlx::Error> {
    if records.is_empty() {
        return Ok(Vec::new());
    }
    let committed = insert_rows(&core.services.db, records, tenancy_scope(core)).await?;
    for row in &committed {
        core.services.buses.audit_bus.publish(row.clone());
    }
    Ok(committed)
}

/// The dashboard a row is scoped to, read from boot at call time.
///
/// A row with no scope is still a row, and an unscoped audit log beats none -- so
/// a coordinator that reached its listener without a tenant writes the row and
/// says so in the log, rather than refusing the request.
fn tenancy_scope(core: &CoordCore) -> Option<String> {
    match core.services.boot.tenant.as_ref() {
        Some(tenant) => Some(tenant.dashboard_id.clone()),
        None => {
            log_error(AUDIT_TARGET, SCOPE_MISSING, LogFields::new().set("fact", "tenant"));
            None
        }
    }
}

async fn insert_rows(
    database: &CoordDb,
    records: &[AuditRecord],
    dashboard_id: Option<String>,
) -> Result<Vec<AuditRow>, sqlx::Error> {
    // One instant for the whole batch: rows written together must not straddle a
    // millisecond, or a read ordered by (ts, id) disagrees with the bus order.
    let ts = crate::serve::now_ms();
    let mut statement = QueryBuilder::<Sqlite>::new(
        "INSERT INTO audit_log (ts, caller_fp, dashboard_id, method, path, status, trace_id) ",
    );
    statement.push_values(records, |mut row, record| {
        row.push(ts)
            .push(record.caller_fp.clone())
            .push(dashboard_id.clone())
            .push(record.method.as_str())
            .push(record.path.as_str())
            .push(i64::from(record.status))
            .push(record.trace_id.clone());
    });
    statement.push(" RETURNING id, ts, caller_fp, method, path, status, trace_id");
    let rows = statement.build().fetch_all(database.pool()).await?;
    let mut committed = rows
        .into_iter()
        .map(|row| AuditRow {
            id: row.get::<i64, _>("id"),
            ts: row.get::<i64, _>("ts"),
            caller_fp: row.get("caller_fp"),
            caller_label: None,
            method: row.get("method"),
            path: row.get("path"),
            status: row.get("status"),
            trace_id: row.get("trace_id"),
        })
        .collect::<Vec<AuditRow>>();
    committed.sort_by_key(|row| row.id);
    Ok(committed)
}
