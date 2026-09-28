//! One session's slice of the coordinator half of a DiagSnapshot: the
//! cached-or-durable worker route, the owner-published terminal view, the
//! screen replica's watermark, and the per-view geometry inputs the effective
//! size was minimized over.
//! Ports `apps/coord/src/diagnostics/diag-snapshot-session-state.ts`
//! (`coordSessionDiagnostic`). Called by `diagnostics::diag_snapshot`; reads
//! `services.byte_hub` and `services.views` and owns no state.
//!
//! `terminal_view` IS THE OWNER ROW OR NULL. v2 falls back to the
//! coordinator-owned stream controller for an unowned session; this port
//! dropped that controller (`terminal_view` module header), so such a session
//! has no stream state and v2's fallback answers `null` there too.

use std::collections::BTreeSet;

use roost_protocol::terminal_view::ViewInput;
use roost_protocol::wire::{SessionId, WorkerFp};
use serde_json::{Value, json};

use crate::services::CoordServices;

/// One durable open session row: `sessions.id`, `worker_fp`, `channel`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagSessionRow {
    pub id: String,
    pub worker_fp: WorkerFp,
    pub channel: i64,
}

/// The durable admission the handler already resolved. Volatile registry
/// state never widens it.
#[derive(Debug, Clone, Copy)]
pub struct DiagSessionScope<'a> {
    /// Workers this caller may see at all.
    pub allowed_worker_fps: &'a BTreeSet<WorkerFp>,
    /// Of those, the ones that can be dispatched to right now.
    pub dispatchable_worker_fps: &'a BTreeSet<WorkerFp>,
}

/// `coord.sessions[id]`: `route`, `terminal_view`, `terminal_screen`, `viewers`.
#[must_use]
pub fn coord_session_diagnostic(
    services: &CoordServices,
    row: &DiagSessionRow,
    scope: DiagSessionScope<'_>,
    now_ms: u64,
) -> Value {
    let session_id = SessionId::try_from(row.id.as_str()).ok();
    let owner_row = session_id
        .as_ref()
        .and_then(|session_id| services.views.owners().row(session_id));
    let terminal_view = owner_row.as_ref().map_or(Value::Null, |owned| {
        let parked = owned.viewers.iter().filter(|viewer| viewer.parked).count();
        json!({
            "activeViews": owned.viewers.len() - parked,
            "parkedViews": parked,
            "streamId": owned.stream_id,
            "effective": owned.effective.map(|geometry| json!({
                "cols": geometry.cols,
                "rows": geometry.rows,
            })),
            // The owning worker publishes the stream it holds; the coordinator
            // drives no desire for it and so has no failure of its own.
            "unavailable": false,
        })
    });
    let viewers: Vec<Value> = match (&owner_row, &session_id) {
        (Some(owned), _) => owned.viewers.iter().map(view_input_json).collect(),
        (None, Some(session_id)) => services
            .views
            .viewer_inputs(session_id, now_ms)
            .iter()
            .map(view_input_json)
            .collect(),
        (None, None) => Vec::new(),
    };
    json!({
        "route": route_json(services, session_id.as_ref(), row, scope),
        "terminal_view": terminal_view,
        "terminal_screen": session_id
            .as_ref()
            .map_or(Value::Null, |session_id| screen_json(services, session_id)),
        "viewers": viewers,
    })
}

fn route_json(
    services: &CoordServices,
    session_id: Option<&SessionId>,
    row: &DiagSessionRow,
    scope: DiagSessionScope<'_>,
) -> Value {
    let cached = session_id.and_then(|session_id| services.byte_hub.cached_route(session_id));
    if let Some(cached) =
        cached.filter(|cached| scope.allowed_worker_fps.contains(&cached.worker_fp))
    {
        return json!({
            "worker_fp": cached.worker_fp.as_str(),
            "channel_id": cached.channel_id.as_u32(),
            "connected": scope.dispatchable_worker_fps.contains(&cached.worker_fp),
            "source": "live_cache",
        });
    }
    if scope.allowed_worker_fps.contains(&row.worker_fp) {
        return json!({
            "worker_fp": row.worker_fp.as_str(),
            "channel_id": row.channel,
            "connected": scope.dispatchable_worker_fps.contains(&row.worker_fp),
            "source": "database",
        });
    }
    Value::Null
}

/// v2 `terminalScreenSnapshot`: null unless the replica both expects a stream
/// and holds a baseline; `seq` is a decimal string so JSON cannot round it.
fn screen_json(services: &CoordServices, session_id: &SessionId) -> Value {
    let sessions = services.byte_hub.screens().locked_sessions();
    let Some(screen) = sessions.get(session_id) else {
        return Value::Null;
    };
    let (Some(expected), Some(cache)) = (screen.expected.as_ref(), screen.charge.current()) else {
        return Value::Null;
    };
    json!({
        "stream_id": expected.stream_id,
        "grid_epoch": cache.frame.grid_epoch,
        "seq": cache.frame.seq.to_string(),
        "cols": cache.frame.cols,
        "rows": cache.frame.rows,
        "valid": cache.valid,
    })
}

/// v2 `TerminalViewInput`, camelCase as the SPA and the smoke probe read it.
fn view_input_json(input: &ViewInput) -> Value {
    json!({
        "fingerprint": input.fingerprint,
        "viewId": input.view_id,
        "cols": input.cols,
        "rows": input.rows,
        "parked": input.parked,
        "constrains": input.constrains,
    })
}
