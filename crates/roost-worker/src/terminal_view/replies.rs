//! How the terminal view owner answers one view command once its geometry is
//! recomputed: v2's per-path tail (admit, reclaim, same- or new-revision
//! update), the unavailable replay, and the view-state frames themselves. Ports
//! `packages/protocol/src/terminal-view/terminal-view-registry-{commands,operations}.ts`
//! reply halves, which `roost_protocol::terminal_view` returns as effects for
//! the host; called from `state.rs` and `streams.rs`.

use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::{TerminalViewCommand, TerminalViewStateFrame, TerminalViewStatus};
use roost_protocol::terminal_view::{PendingReply, view_key, view_state_frame};
use roost_protocol::wire::brand::SessionId;

use super::deferred::{Deferred, Work};
use super::state::OwnerState;

/// Which v2 command path a view command takes. The registry's outcome does not
/// say, and each path finishes differently once the geometry is recomputed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CommandPath {
    Admit,
    Reclaim,
    SameRevision,
    NewRevision,
}

impl OwnerState {
    pub(super) fn command_path(
        &self,
        socket_id: &str,
        command: &TerminalViewCommand,
    ) -> CommandPath {
        let Some(viewer_key) = self
            .registry
            .socket(socket_id)
            .and_then(|socket| socket.viewer_key.clone())
        else {
            return CommandPath::Admit;
        };
        match self.registry.view(&view_key(&viewer_key, &command.view_id)) {
            None => CommandPath::Admit,
            Some(record) if record.socket_id != socket_id => CommandPath::Reclaim,
            Some(record) if command.revision == record.revision => CommandPath::SameRevision,
            Some(_) => CommandPath::NewRevision,
        }
    }

    /// v2's per-path tail once the recompute has run. `broadcast` is the
    /// recompute's "the decision already reached every live view".
    pub(super) fn finish_reply(
        &mut self,
        reply: &PendingReply,
        path: CommandPath,
        broadcast: bool,
        session_id: Option<SessionId>,
        work: &mut Work,
    ) {
        let PendingReply::View { socket_id, .. } = reply else {
            self.reply_command(reply);
            return;
        };
        let Some(session_id) = session_id else {
            return;
        };
        let unavailable = self
            .streams
            .get(&session_id)
            .is_some_and(|stream| stream.unavailable);
        let socket_id = socket_id.clone();
        match path {
            CommandPath::Admit if !broadcast => {
                self.reply_view(reply);
                self.seed(&socket_id, &session_id, work);
            }
            CommandPath::Reclaim if !broadcast => {
                if unavailable {
                    self.replay_unavailable(reply, &session_id, work);
                    return;
                }
                self.reply_view(reply);
                self.seed(&socket_id, &session_id, work);
            }
            CommandPath::SameRevision => {
                if unavailable {
                    self.replay_unavailable(reply, &session_id, work);
                } else {
                    self.reply_view(reply);
                }
                work.push(Deferred::EnsureStream {
                    socket_id,
                    session_id,
                });
            }
            CommandPath::NewRevision if !broadcast => self.reply_view(reply),
            CommandPath::Admit | CommandPath::Reclaim | CommandPath::NewRevision => {}
        }
    }

    /// v2 `seedSocket`, which `deferred.rs` performs once the lock is released.
    fn seed(&self, socket_id: &str, session_id: &SessionId, work: &mut Work) {
        work.push(Deferred::Seed {
            socket_id: socket_id.to_owned(),
            session_id: session_id.clone(),
        });
    }

    /// v2 `replayUnavailable`: a heartbeat-policy session is redriven and the
    /// next decision answers; anything else replays UNAVAILABLE.
    fn replay_unavailable(
        &mut self,
        reply: &PendingReply,
        session_id: &SessionId,
        work: &mut Work,
    ) {
        let Some(stream) = self.streams.get(session_id) else {
            self.reply_view(reply);
            return;
        };
        if !stream.unavailable {
            self.reply_view(reply);
            return;
        }
        if stream.policy_is_heartbeat() {
            self.redrive(session_id, work);
            return;
        }
        let reason = stream.unavailable_reason.clone();
        self.reply_view(&with_status(
            reply,
            TerminalViewStatus::Unavailable,
            &reason,
        ));
    }

    /// v2 `replyView`: a record's answer carries the session's stream and
    /// effective geometry, so there is none before a geometry exists.
    pub(super) fn reply_view(&self, reply: &PendingReply) {
        let PendingReply::View {
            socket_id,
            view_id,
            session_id,
            revision,
            status,
            reason,
        } = reply
        else {
            return;
        };
        if self.registry.socket(socket_id).is_none() {
            return;
        }
        let Some(stream) = SessionId::try_from(session_id.clone())
            .ok()
            .and_then(|id| self.streams.get(&id))
        else {
            return;
        };
        let Some(effective) = stream.effective else {
            return;
        };
        let frame = view_state_frame(
            view_id,
            session_id,
            *revision,
            true,
            &stream.stream_id,
            *status,
            effective.cols,
            effective.rows,
            reason,
        );
        self.send(socket_id, frame.frame);
    }

    /// v2 `replyCommand`: a refusal or an inactive acknowledgement carries no
    /// stream and no geometry.
    fn reply_command(&self, reply: &PendingReply) {
        let PendingReply::Command {
            socket_id,
            view_id,
            session_id,
            revision,
            active,
            status,
            reason,
        } = reply
        else {
            return;
        };
        let frame = view_state_frame(
            view_id, session_id, *revision, *active, "", *status, 0, 0, reason,
        );
        self.send(socket_id, frame.frame);
    }

    /// v2's `stateSink`: only the view-state member of the firehose frame the
    /// shared builder produces goes to the transport.
    fn send(&self, socket_id: &str, frame: Option<Frame>) {
        if let Some(Frame::TerminalViewState(state)) = frame {
            let state: TerminalViewStateFrame = *state;
            self.screen.send_view_state(socket_id, state, &self.uplink);
        }
    }
}

/// The same record answer with a different decision.
fn with_status(reply: &PendingReply, status: TerminalViewStatus, reason: &str) -> PendingReply {
    let mut changed = reply.clone();
    if let PendingReply::View {
        status: slot,
        reason: text,
        ..
    } = &mut changed
    {
        *slot = status;
        reason.clone_into(text);
    }
    changed
}
