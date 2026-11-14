//! Which request outcomes are worth a durable `audit_log` row, and the status a
//! refused Connect RPC is recorded with.
//!
//! Owned by the audit slice, split from [`crate::middleware::audit`] so the
//! policy and the write can be read and changed apart. Nothing here touches the
//! database or the bus: these are pure decisions, and a caller that wants the
//! row itself goes to `record_request`.

use connectrpc::ErrorCode;

use crate::coord_core::ListenerTrust;

/// The non-Connect surfaces the listener serves.
///
/// Connect is deliberately not a variant: only [`crate::middleware::audit::AuditRecord::connect`]
/// builds a Connect row, so the outer layer cannot write one for a request whose
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

/// Whether a non-Connect outcome is worth a durable row.
///
/// "an unmatched /api/* path is an unauthenticated GET the rate limiter lets
/// through, so one durable row per probed path is pure amplification" -- and the
/// janitor only sweeps anonymous *static* reads, so an amplified row is not even
/// self-cleaning (`apps/coord/src/middleware/security.ts:114`).
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
/// unbounded volume with no forensic value -- and the retention sweep never ages
/// out auth rows, so such a row is permanent. That table once reached 7,026,358
/// rows / 1.0 GB (`docs/FAILURE-INDEX.md`). The same 401 on a directly-observed
/// listener persists: that caller is low volume and names a host, not a stranger.
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
///
/// The catch-all is the source's own `default: 500`: `ErrorCode` is
/// `#[non_exhaustive]`, so a code Connect adds after this port is an outcome
/// nobody has decided how to record, and 500 says exactly that.
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
        _ => 500,
    }
}
