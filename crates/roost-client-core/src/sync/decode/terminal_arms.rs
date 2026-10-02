//! The terminal-domain arms: session events (JSON and typed), session presence,
//! cell chunks, view states, last activity and agent status.
//!
//! Called by `decode::map_arm` after the meta rule has passed. Ported from
//! `apps/web/src/store/sync-frame.ts:108-137` (sessions), `:231-262`
//! (presence), `:270-276` (last activity) and `apps/web/src/store/agent-status.ts:213-235`.

use roost_proto::{
    AgentStatusFrame, LastActivityFrame, PbCellGridChunk, SessionEventProto, SessionPresence,
    TerminalViewStateFrame, TerminalViewStatus,
};
use roost_protocol::wire::{AgentStatusUpdate, SessionEvent, proto_to_event};
use serde_json::{Value, json};

use crate::sessions::WireEvent;
use crate::sync::inbound::{SessionViewer, SyncFrame};

/// A legacy JSON session event.
///
/// Unparseable JSON is deliberately fatal: a sessions payload the coordinator
/// itself persisted must parse, and folding garbage would poison the store far
/// worse than one reconnect (`sync-frame.ts:111-129`). JSON that parses but is
/// not a session event still moves the cursor and folds nothing, as v2 moves
/// `_lastSeenEventId` before `foldEventIntoStore` rejects the shape.
pub(super) fn sessions_json(payload_json: &str) -> Result<SyncFrame, String> {
    let value: Value = serde_json::from_str(payload_json)
        .map_err(|error| format!("payload_json is not JSON: {error}"))?;
    let event_id = value
        .get("_event_id")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    Ok(match SessionEvent::parse(value) {
        Ok(event) => SyncFrame::SessionEvent {
            event: WireEvent(event),
            event_id,
        },
        Err(error) => SyncFrame::SessionEventRejected {
            event_id,
            reason: error.to_string(),
        },
    })
}

/// A typed session event. v2's `protoToEvent` returns null for a kind it does
/// not know (dispatch "unapplied") and throws for a malformed one; both close
/// the link.
pub(super) fn session_event(value: &SessionEventProto) -> Result<SyncFrame, String> {
    match proto_to_event(value) {
        Ok(Some(decoded)) => Ok(SyncFrame::SessionEvent {
            event: WireEvent(decoded.event),
            event_id: decoded.event_id,
        }),
        Ok(None) => Err("session_event carries no kind this build knows".to_owned()),
        Err(error) => Err(error.to_string()),
    }
}

/// A presence notice: a viewer list when its kind is `viewers`, else opaque.
/// Unparseable JSON is fatal, as in v2 (`sync-frame.ts:256-260`).
pub(super) fn session_presence(value: SessionPresence) -> Result<SyncFrame, String> {
    let payload: Value = serde_json::from_str(&value.payload_json)
        .map_err(|error| format!("payload_json is not JSON: {error}"))?;
    if payload.get("kind").and_then(Value::as_str) == Some("viewers") {
        return Ok(SyncFrame::SessionViewers {
            session_id: value.session_id,
            viewers: viewers_of(&payload),
        });
    }
    Ok(SyncFrame::SessionPresence {
        session_id: value.session_id,
        payload,
    })
}

/// v2 reads `entries` when present, else the bare `fps` list, else nothing;
/// it validates neither, so a missing field is a default here, not a refusal.
fn viewers_of(payload: &Value) -> Vec<SessionViewer> {
    if let Some(entries) = payload.get("entries").and_then(Value::as_array) {
        return entries.iter().map(viewer_entry).collect();
    }
    let Some(fps) = payload.get("fps").and_then(Value::as_array) else {
        return Vec::new();
    };
    fps.iter()
        .map(|fp| {
            let fp = fp.as_str().unwrap_or_default().to_owned();
            SessionViewer {
                viewer_key: fp.clone(),
                fp,
                cols: 0,
                rows: 0,
                last_ms: None,
                label: None,
            }
        })
        .collect()
}

fn viewer_entry(entry: &Value) -> SessionViewer {
    let fp = text_field(entry, "fp").unwrap_or_default();
    SessionViewer {
        viewer_key: text_field(entry, "viewerKey").unwrap_or_else(|| fp.clone()),
        fp,
        cols: dimension(entry, "cols"),
        rows: dimension(entry, "rows"),
        last_ms: entry.get("lastMs").and_then(Value::as_i64),
        label: text_field(entry, "label"),
    }
}

fn text_field(entry: &Value, key: &str) -> Option<String> {
    entry.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn dimension(entry: &Value, key: &str) -> u32 {
    entry
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or_default()
}

/// One part of a chunked baseline, routed by the session its part names.
pub(super) fn cell_grid_chunk(value: PbCellGridChunk) -> SyncFrame {
    let session_id = value
        .part
        .as_option()
        .map(|part| part.session_id.clone())
        .unwrap_or_default();
    SyncFrame::CellGridChunk {
        session_id,
        chunk: value,
    }
}

/// A view-state answer, correlated by the envelope's domain generation: the
/// view-state message has no generation of its own, and v2 correlates it with
/// the terminal domain's current one after `dispatchV2Application` has matched
/// the envelope against it.
pub(super) fn view_state(value: TerminalViewStateFrame, domain_generation: u64) -> SyncFrame {
    SyncFrame::ViewState {
        accepted: value.status.as_known() == Some(TerminalViewStatus::Accepted),
        session_id: value.session_id,
        view_id: value.view_id,
        generation: domain_generation,
        revision: value.revision,
        stream_id: value.stream_id,
        effective_cols: value.effective_cols,
        effective_rows: value.effective_rows,
    }
}

/// The last-activity stamp. The wire carries a double so a JS client keeps a
/// number; the store keeps whole milliseconds.
pub(super) fn last_activity(value: LastActivityFrame) -> SyncFrame {
    SyncFrame::LastActivity {
        session_id: value.session_id,
        ts_ms: value.ts_ms as i64,
    }
}

/// An agent-status report through the shared schema, as v2's
/// `AgentStatusUpdate.safeParse`. A refusal drops the report and keeps the
/// link: v2 ignores `applyAgentStatusFrame`'s return (`sync-frame.ts:274-276`).
pub(super) fn agent_status(value: AgentStatusFrame) -> SyncFrame {
    let candidate = json!({
        "session_id": value.session_id,
        "agent_id": value.agent_id,
        "state": value.state,
        "message": value.message,
        "revision": value.revision,
        "completed_revision": value.completed_revision,
        "updated_at": whole_number(value.updated_at),
        "active": value.active,
        "status_epoch": value.status_epoch,
        "occupant_id": value.occupant_id,
        "source": value.source,
        "occupant_exited": value.occupant_exited,
    });
    match AgentStatusUpdate::parse(candidate) {
        Ok(update) => SyncFrame::AgentStatus { update },
        Err(error) => SyncFrame::AgentStatusRefused {
            session_id: value.session_id,
            reason: error.to_string(),
        },
    }
}

/// A double that holds a whole number, as the integer JSON the schema's
/// integer field accepts; any other double stays a double and is refused by it.
fn whole_number(value: f64) -> Value {
    let whole = value.trunc();
    if value.is_finite() && whole == value && whole.abs() < 9_007_199_254_740_992.0 {
        Value::from(whole as i64)
    } else {
        Value::from(value)
    }
}
