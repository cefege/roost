//! `CoordinatorService.DiagDebugLogBatch` — the sink a browser uploads its
//! own diagnostics into.
//!
//! Ported from `handlers-system.ts:142-181`. A browser batches its diag events
//! and ships them every 100 ms or 64 entries; this re-emits each one through
//! the coordinator's log so a terminal-corruption investigation has the SPA's
//! view and the coordinator's view in ONE file.
//!
//! TWO TIERS, AND THE GATE IS THE COORDINATOR'S, NOT THE BROWSER'S. A Tier-1
//! signal is an anomaly or an error and always lands, on the `signal` target
//! that `roost doctor` reads. The Tier-0 firehose — every `info` entry — is
//! dropped unless `ROOST_DIAG=1` here. v2's note is the reason: "gate, don't
//! chase every browser's localStorage", and a stale browser that still has the
//! flag set must not be able to flood the operator's disk from the other side
//! of the process.
//!
//! THE ENTRY'S OWN `evt` SURVIVES ITS `kv`. `spa.uncaught` carries the error
//! text in `kv.msg`, which would otherwise clobber the structural message and
//! leave `roost doctor` grouping a day's failures under a browser's own words.
//! `roost doctor` groups by `evt`, so the `evt` is written last and flat.

use connectrpc::ServiceResult;
use roost_observability::diag::is_diag_enabled;
use roost_proto as proto;
use tracing::{error, info};

use crate::auth::principal::require_account_device;
use crate::coord_core::{Caller, CoordCore};
use crate::rpc::service::ok_response;

/// The target a Tier-1 signal is logged under: the always-on channel.
const SIGNAL_TARGET: &str = "signal";

/// The target the gated firehose is logged under, so a grep for diagnostics
/// stays independent of operational logs.
const DIAG_TARGET: &str = "diag";

/// Where every entry's own `src` comes from. The coordinator never accepts a
/// browser's claim about its origin, because there is only one.
const SOURCE: &str = "spa";

/// `CoordinatorService.DiagDebugLogBatch`.
pub async fn handle_diag_debug_log_batch(
    _core: &CoordCore,
    caller: &Caller,
    request: proto::DiagDebugLogBatchRequest,
) -> ServiceResult<proto::DiagDebugLogBatchResponse> {
    require_account_device(caller)?;
    let firehose_on = is_diag_enabled();
    let mut accepted = 0_u32;
    for entry in &request.entries {
        if !entry.signal && !firehose_on {
            continue;
        }
        emit(entry);
        accepted = accepted.saturating_add(1);
    }
    ok_response(proto::DiagDebugLogBatchResponse {
        accepted,
        ..Default::default()
    })
}

/// Re-emit one entry under the target its tier names.
///
/// A malformed `kv_json` is dropped rather than failing the batch: one bad
/// entry from one browser must not cost the other sixty-three, and the entry's
/// own `evt`, `sid` and trace id are what an investigation actually reads.
fn emit(entry: &proto::DiagDebugLogEntry) {
    let fields = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&entry.kv_json)
        .unwrap_or_default();
    let empty = String::new();
    let sid = non_empty(&entry.sid).unwrap_or(&empty);
    let viewer_key = non_empty(&entry.viewer_key).unwrap_or(&empty);
    let trace_id = non_empty(&entry.trace_id).unwrap_or(&empty);
    let session_trace_id = non_empty(&entry.session_trace_id).unwrap_or(&empty);
    // `evt` is the last field, so a `kv` of the same name cannot displace it.
    let kv = fields
        .into_iter()
        .map(|(key, value)| (format!("kv_{key}"), tracing::field::debug(value)))
        .collect::<Vec<_>>();
    if entry.signal {
        error!(
            target: SIGNAL_TARGET,
            evt = %entry.evt,
            ts_spa = entry.ts_ms,
            mono_ns = entry.mono_ns,
            %trace_id,
            %session_trace_id,
            %sid,
            %viewer_key,
            src = SOURCE,
            ?kv,
            "spa diagnostic signal"
        );
    } else {
        info!(
            target: DIAG_TARGET,
            evt = %entry.evt,
            ts_spa = entry.ts_ms,
            mono_ns = entry.mono_ns,
            %trace_id,
            %session_trace_id,
            %sid,
            %viewer_key,
            src = SOURCE,
            ?kv,
            "spa diagnostic"
        );
    }
}

/// An optional wire string that is present but empty is absent: v2 writes
/// `e.sid || undefined`, and a log line reading `sid=""` is a different claim
/// from one with no `sid` at all.
fn non_empty(value: &str) -> Option<&str> {
    if value.is_empty() { None } else { Some(value) }
}
