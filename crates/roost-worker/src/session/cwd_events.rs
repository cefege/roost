//! OSC 7 moved a session to a new folder → one `cwd` session event: v2
//! `apps/worker/src/session/session-scrollback.ts:178-185` (`scanStreamState`
//! emits `{ kind: "cwd" }` on a new path) through `transport/event-sink.ts`
//! (metadata, no claim). The ingest path (`session::emit_ingest`, keeper
//! dispatch thread, record and emitter locked) sends on a [`CwdEventLane`];
//! the [`CwdEventWriter`] task `runtime::owners` spawns publishes in order.

use std::sync::Arc;

use roost_protocol::wire::brand::SessionId;
use roost_protocol::wire::event::SessionEvent;
use tokio::sync::mpsc;

use super::lifecycle::SessionManager;

/// The sending half: synchronous and non-blocking, so the ingest path can
/// hand a change over with its locks held.
#[derive(Debug, Clone, Default)]
pub struct CwdEventLane {
    /// `None` only for a lane nothing drains (a detached emitter).
    sender: Option<mpsc::UnboundedSender<SessionEvent>>,
}

impl CwdEventLane {
    /// A lane and the writer that must run for its events to be published.
    pub fn new() -> (Self, CwdEventWriter) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (
            Self {
                sender: Some(sender),
            },
            CwdEventWriter { receiver },
        )
    }

    /// Queue the `cwd` event for a folder change the scan just recorded.
    /// `false` when no writer will publish it; each such drop is logged.
    pub fn send(&self, session_id: &SessionId, cwd: &str, now_ms: i64) -> bool {
        let event = SessionEvent::Cwd {
            session_id: session_id.clone(),
            cwd: cwd.to_owned(),
            ts: now_ms,
            trace_id: None,
        };
        let queued = self
            .sender
            .as_ref()
            .is_some_and(|sender| sender.send(event).is_ok());
        if !queued {
            tracing::warn!(%session_id, cwd, "a cwd change was not published: no cwd event writer is running");
        }
        queued
    }
}

/// The receiving half: publishes every queued `cwd` event, in order.
#[derive(Debug)]
pub struct CwdEventWriter {
    receiver: mpsc::UnboundedReceiver<SessionEvent>,
}

impl CwdEventWriter {
    /// Publish until every [`CwdEventLane`] is dropped. A refused publish is
    /// logged and the next event still goes: the coordinator's projection of
    /// the folder is replaced by the next change or the next snapshot.
    pub async fn run(mut self, manager: Arc<SessionManager>) {
        tracing::info!("the cwd event writer started");
        while let Some(event) = self.receiver.recv().await {
            match manager.events.emit(&event, None).await {
                Ok(()) => {
                    tracing::info!(session_id = ?event.session_id(), "a session's new working folder was published")
                }
                Err(error) => tracing::warn!(
                    session_id = ?event.session_id(),
                    %error,
                    "a session's new working folder could not be published"
                ),
            }
            // v2 session-scrollback.ts:186-190: a new folder may be another repo — re-watch it.
            if let Some(session_id) = event.session_id()
                && let Some(channel_id) = manager.sessions.channel_of(session_id)
            {
                manager.notify_session_folder(session_id, channel_id);
            }
        }
        tracing::info!("the cwd event writer stopped: every lane was dropped");
    }
}
