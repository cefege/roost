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

/// Whether the own-echo rule can bite this payload: a per-viewer presence
/// notice, read for a socket that HAS a viewer identity.
///
/// This is the applicability test, not the decision. It is true for another
/// viewer's notice exactly as it is for the socket's own, because both are the
/// kind that names a `viewer_id`; telling them apart is
/// [`presence_echo_is_own_notice`]. `viewers` is a snapshot of the room rather
/// than a notice from one viewer, so the rule never applies to it however many
/// ids it lists. A socket with no `viewer_key` has no own notice to suppress.
#[must_use]
pub fn presence_is_viewer_addressed(data: &Value, viewer_key: Option<&str>) -> bool {
    if viewer_key.is_none() {
        return false;
    }
    let Some(kind) = data.get("kind").and_then(Value::as_str) else {
        return false;
    };
    matches!(kind, "presence-delta" | "presence-leave")
}

/// Whether this payload is the VIEWER'S OWN notice, the one notice a socket
/// must not receive back: echoing a viewer's own cursor to it makes its own tab
/// appear to join and leave.
///
/// This is the whole of the subscriber's presence refusal
/// (`sync-feed.ts:236-242`): the single condition under which a session's
/// presence is not pushed to a socket watching that session. Another viewer's
/// notice is the feature and is delivered to every viewer.
///
/// `viewer_key` is an `Option` on both sides of that equality, so a payload
/// that names a viewer can never match a socket that has none. The gate has
/// already answered that case, and spelling it this way keeps the comparison
/// false if the two are ever reordered.
#[must_use]
pub fn presence_echo_is_own_notice(data: &Value, viewer_key: Option<&str>) -> bool {
    presence_is_viewer_addressed(data, viewer_key)
        && data.get("viewer_id").and_then(Value::as_str) == viewer_key
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
