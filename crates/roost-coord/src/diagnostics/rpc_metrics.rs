//! `CoordinatorService.MiscMetrics` — the counters this process keeps.
//!
//! Ported from `handlers-system.ts:122-135` over `diagnostics::telemetry`,
//! which owns the numbers. The handler's whole job is the authority check and
//! the conversion: the counts are already bounded, already folded, and already
//! per-process, so there is nothing here for a caller to influence except by
//! making a request.
//!
//! IT IS DEVICE-AUTHENTICATED AND NOT PUBLIC, unlike `MiscHealth`. A metric is
//! a map of request paths and a count of who asked for them; both are the
//! operator's business and neither is a stranger's.

use connectrpc::ServiceResult;
use roost_proto as proto;

use crate::auth::principal::require_account_device;
use crate::coord_core::{Caller, CoordCore};
use crate::rpc::service::ok_response;

/// `CoordinatorService.MiscMetrics`.
pub async fn handle_misc_metrics(
    core: &CoordCore,
    caller: &Caller,
    _request: proto::MiscMetricsRequest,
) -> ServiceResult<proto::MiscMetricsResponse> {
    require_account_device(caller)?;
    let snapshot = core.services.telemetry.snapshot();
    ok_response(proto::MiscMetricsResponse {
        uptime_ms: snapshot.uptime_ms,
        requests: snapshot.requests.into_iter().collect(),
        errors: snapshot.errors.into_iter().collect(),
        total_requests: snapshot.total_requests,
        total_errors: snapshot.total_errors,
        ..Default::default()
    })
}
