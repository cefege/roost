//! What a viewer is doing right now, and the title it gave a session. Owned by
//! the worker.
//!
//! Three commands, and the property they share is the reason they are one
//! module: none of them asks for an answer. A cursor position, a title and a
//! detach are all statements about a moment that has already passed, and a
//! browser that has moved on is not waiting to hear about any of them. So the
//! dispatch answers them with silence, deliberately, and the coordinator is
//! the side that decides not to register a pending entry for them.
//!
//! The consequence is that a failure here cannot be reported to the caller.
//! It is logged, once, and the state is left as it was: a title that could not
//! be set is a title the next one will set, and a cursor position the worker
//! missed is a position the browser will send again as soon as it moves. What
//! must never happen is the opposite — a partly-applied presence update left
//! behind by a failure halfway through, which is why each of these is a single
//! call rather than a sequence this module could interrupt.

use roost_protocol::wire::brand::SessionId;
use roost_protocol::wire::control::ClientControlFrame;

use super::{Answered, Command, Deps, Refusal};

/// Where a viewer's cursor is, and the title a session is being shown under.
pub trait PresenceReports: Send + Sync {
    /// A viewer's cursor moved within a session it is watching.
    fn cursor_moved(&self, session_id: SessionId, col: u16, row: u16);

    /// A viewer named a session.
    fn titled(&self, session_id: SessionId, title: String);

    /// A viewer stopped watching a session.
    fn viewer_left(&self, session_id: SessionId, browser_id: String);
}

/// Run whichever presence command arrived. None of them is answered.
pub async fn execute(command: &Command, deps: &Deps) -> Result<Answered, Refusal> {
    let presence = deps.presence.as_ref();
    match &command.frame {
        ClientControlFrame::CursorPos {
            session_id,
            col,
            row,
            ..
        } => {
            presence.cursor_moved(session_id.clone(), clamp(*col), clamp(*row));
        }
        ClientControlFrame::SetTitle {
            session_id, title, ..
        } => {
            presence.titled(session_id.clone(), title.clone());
        }
        ClientControlFrame::Detach { session_id, .. } => {
            presence.viewer_left(session_id.clone(), command.browser_id.clone());
        }
        other => {
            return Err(Refusal::failed(
                "presence",
                format!("{} is not a presence command", other.kind()),
            ));
        }
    }
    Ok(Answered::Silent)
}

/// A column or row the wire admitted as non-negative, narrowed to what a grid
/// can address.
fn clamp(value: i64) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}
