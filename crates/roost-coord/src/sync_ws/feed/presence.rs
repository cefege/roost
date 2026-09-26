//! Session-scoped presence: re-keying a worker's opaque presence payload by
//! session, and the one viewer that must not receive its own echo.
//!
//! Ported from `apps/coord/src/sync/presence-hub.ts` plus the payload filter
//! at `sync-feed.ts:234-248`. A worker's `presence` frame names a CHANNEL, not
//! a session, and the SPA has one Sync subscription rather than one presence
//! EventSource per terminal -- so the coordinator resolves the channel to its
//! session here and republishes onto the global presence bus. A payload whose
//! channel no live session owns is dropped at the door, which is the only
//! moment a stale channel can be caught.
//!
//! PRESENCE IS BROADCAST, WITH ONE EXCEPTION. Every viewer of a session learns
//! that another viewer arrived, moved or left -- that is the whole feature. The
//! exception is the viewer's OWN cursor and leave notices: echoing a viewer's
//! own presence back to it makes its own tab appear to join and leave, so those
//! two kinds are dropped for the socket that authored them. The payload is
//! opaque by construction, so the filter reads two keys and nothing else.

use serde_json::Value;

use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::{FirehoseFrame, SessionPresence};

use crate::coord_core::seams::WorkerRouteIndex;
use crate::events::bus_domains::Buses;
use crate::events::bus_messages::SessionPresenceUpdate;
use crate::sync_ws::feed::FeedFrame;
use roost_protocol::wire::{ChannelId, SessionId, WorkerFp};

/// One presence observation as its frame.
pub fn session_presence_frame(update: &SessionPresenceUpdate) -> FeedFrame {
    FeedFrame::of(FirehoseFrame {
        frame: Some(Frame::SessionPresence(Box::new(SessionPresence {
            session_id: update.session_id.clone(),
            payload_json: payload_text(&update.data),
            ..SessionPresence::default()
        }))),
        ..FirehoseFrame::default()
    })
}

/// Whether this payload is the VIEWER'S OWN notice, which that viewer must not
/// receive back: echoing a viewer's own cursor to it makes its tab appear to
/// join and leave.
///
/// `viewer_key` of `None` is a socket with no viewer identity, which cannot be
/// the author of a notice that names one, so nothing is dropped for it.
#[must_use]
pub fn presence_is_addressed_to_another_viewer(data: &Value, viewer_key: Option<&str>) -> bool {
    let Some(viewer_key) = viewer_key else {
        return false;
    };
    let Some(kind) = data.get("kind").and_then(Value::as_str) else {
        return false;
    };
    if !matches!(kind, "presence-delta" | "presence-leave") {
        return false;
    }
    data.get("viewer_id").and_then(Value::as_str) == Some(viewer_key)
}

/// Publish one relayed presence payload, keyed by the session its channel
/// currently carries, and name the session it was keyed by.
///
/// `None` means the channel resolved to no live session: the payload is
/// dropped, because a presence notice about a session that does not exist is
/// indistinguishable from one about a session the reader has not been told
/// about yet.
pub fn publish_presence(
    buses: &Buses,
    routes: &dyn WorkerRouteIndex,
    worker_fp: &WorkerFp,
    channel_id: ChannelId,
    payload: Value,
) -> Option<SessionId> {
    let session_id = routes.lookup_session_id(worker_fp, &channel_id)?;
    buses.global_presence_bus.publish(SessionPresenceUpdate {
        session_id: session_id.as_str().to_owned(),
        data: payload,
    });
    tracing::debug!(
        event = "sync.presence.published",
        session_id = session_id.as_str(),
        "relayed worker presence re-keyed onto a session"
    );
    Some(session_id)
}

/// A presence payload as the JSON text the wire field is declared as.
///
/// `serde_json::Value` always serialises, so this cannot fail; a payload that
/// somehow is not a JSON value renders as `null` rather than dropping the frame
/// and taking the rest of the session's presence with it.
fn payload_text(data: &Value) -> String {
    serde_json::to_string(data).unwrap_or_else(|_| String::from("null"))
}
