//! The terminal view command state machine: what one client declaration does
//! to membership. Accept, reject, reclaim, release, and the two capacity
//! refusals, then name the sessions whose effective geometry moved.
//!
//! Ported from `packages/protocol/src/terminal-view/terminal-view-registry-commands.ts`
//! and `terminal-view-registry-operations.ts`.
//!
//! EVERY PATH ENDS IN A RE-MINIMIZE OR AN ANSWER. A declaration that changes
//! membership names its session as changed; one that does not owes the socket a
//! reply: a path that did neither would leave the client waiting and the PTY at
//! a size nobody asked for.

use roost_proto::{TerminalResyncCommand, TerminalViewCommand, TerminalViewStatus};
use roost_protocol::viewport::{TERMINAL_SOCKET_VIEW_CAP, is_terminal_uuid};
use roost_protocol::wire::SessionId;

use super::record::{
    SESSION_VIEW_CAP, ViewRecord, intent_of, intents_equal, validate_view_command, view_key,
};
use super::machine::session_of;
use super::registry::{Machine, MembershipOutcome};
use super::sink::PendingReply;

/// Why a declaration that renames a live handle is refused.
const SESSION_MOVED: &str = "a terminal view cannot change sessions";

/// The authenticated socket a command arrived on, resolved once so every
/// refusal answers the caller and never the record's previous owner.
struct Caller {
    socket_id: String,
    viewer_key: String,
    fingerprint: String,
}

impl Machine<'_> {
    /// Apply one view command from one socket.
    pub(super) fn handle_view_command(
        &mut self,
        socket_id: &str,
        command: &TerminalViewCommand,
        now_ms: u64,
    ) -> MembershipOutcome {
        let mut outcome = MembershipOutcome::default();
        let Some(socket) = self.sockets.get(socket_id) else {
            return outcome;
        };
        let viewer_key = socket.viewer_key.clone();
        let allowed = socket.allows(&command.session_id);
        if let Some(reason) = validate_view_command(viewer_key.as_deref(), command) {
            outcome.refuse(socket_id, command, reason);
            return outcome;
        }
        // Checked before any mutation: the session set is seeded from persisted
        // rows at Sync admission and only expanded by an event this socket owns.
        if !allowed {
            outcome.refuse(socket_id, command, "terminal session is unavailable");
            return outcome;
        }
        let Some(viewer_key) = viewer_key else {
            return outcome;
        };
        let caller = Caller {
            socket_id: socket_id.to_owned(),
            viewer_key,
            fingerprint: socket.fingerprint.clone(),
        };
        let key = view_key(&caller.viewer_key, &command.view_id);
        match self.views.get(&key).cloned() {
            Some(current) if current.socket_id != caller.socket_id => {
                self.reclaim(&caller, command, current, key, now_ms, &mut outcome);
            }
            Some(current) => self.update(&caller, command, current, now_ms, &mut outcome),
            None => self.admit(&caller, command, key, now_ms, &mut outcome),
        }
        outcome
    }

    /// Apply one resync request.
    ///
    /// Whether the requested stream is still the current one is the host's to
    /// answer: it owns the replica, and it can prove a checkpoint is reachable
    /// where this machine knows nothing about baselines.
    pub(super) fn handle_resync(
        &mut self,
        socket_id: &str,
        command: &TerminalResyncCommand,
    ) -> MembershipOutcome {
        let mut outcome = MembershipOutcome::default();
        let Some(socket) = self.sockets.get(socket_id) else {
            return outcome;
        };
        let Some(viewer_key) = socket.viewer_key.clone() else {
            return outcome;
        };
        if !socket.allows(&command.session_id) || !is_terminal_uuid(&command.view_id) {
            return outcome;
        }
        let key = view_key(&viewer_key, &command.view_id);
        let Some(record) = self.views.get(&key) else {
            return outcome;
        };
        if record.socket_id != socket_id
            || record.parked
            || record.intent.session_id != command.session_id
        {
            return outcome;
        }
        let Some(session_id) = session_of(record) else {
            return outcome;
        };
        outcome.calls.push(super::sink::SinkCall::Resync {
            socket_id: socket_id.to_owned(),
            session_id,
            grid_epoch: command.grid_epoch.clone(),
            seq: command.seq,
        });
        outcome
    }

    /// A record whose socket is gone, reclaimed by the same tab on the same
    /// device. The viewer key is `${fingerprint}:${tab}`, so nobody else can
    /// reach this key and the previous owner's socket is provably closed. The
    /// incoming geometry is ADOPTED: a phone that rotated must rejoin the
    /// aggregate now, not after the lease reaps it.
    fn reclaim(
        &mut self,
        caller: &Caller,
        command: &TerminalViewCommand,
        mut current: ViewRecord,
        key: String,
        now_ms: u64,
        outcome: &mut MembershipOutcome,
    ) {
        if !current.parked {
            outcome.refuse(&caller.socket_id, command, "view is owned by another live socket");
            return;
        }
        if command.revision < current.revision {
            outcome.refuse(&caller.socket_id, command, "stale terminal view revision");
            return;
        }
        if command.session_id != current.intent.session_id {
            outcome.refuse(&caller.socket_id, command, SESSION_MOVED);
            return;
        }
        if !command.active {
            self.release(&caller.socket_id, command, &key, now_ms, outcome);
            return;
        }
        if self.view_count(&caller.socket_id) >= TERMINAL_SOCKET_VIEW_CAP {
            outcome.refuse(
                &caller.socket_id,
                command,
                "terminal socket view capacity exceeded",
            );
            return;
        }
        let resized = current.intent.cols != command.cols || current.intent.rows != command.rows;
        let session_id = session_of(&current);
        let previous_socket = current.socket_id.clone();
        current.socket_id = caller.socket_id.clone();
        current.intent = intent_of(command);
        current.revision = command.revision;
        current.deadline_ms = ViewRecord::lease_deadline(now_ms);
        current.parked = false;
        current.parked_at_ms = 0;
        current.constrains = true;
        self.views.insert(key.clone(), current.clone());
        if let Some(socket) = self.sockets.get_mut(&caller.socket_id) {
            socket.views.insert(key);
        }
        if let Some(session_id) = session_id.clone() {
            outcome.changed.insert(session_id.clone());
            outcome.calls.extend(self.sync_watching(&caller.socket_id, &session_id));
        }
        tracing::info!(
            session_id = %current.intent.session_id,
            view_id = %current.view_id,
            socket_id = %caller.socket_id,
            previous_socket = %previous_socket,
            cols = current.intent.cols,
            rows = current.intent.rows,
            resized,
            "terminal view reclaimed by its own tab"
        );
        outcome.answer_view(&current, TerminalViewStatus::Accepted, "");
    }

    /// A declaration from the socket that already owns the record.
    fn update(
        &mut self,
        caller: &Caller,
        command: &TerminalViewCommand,
        mut current: ViewRecord,
        now_ms: u64,
        outcome: &mut MembershipOutcome,
    ) {
        if command.revision < current.revision {
            outcome.refuse(&caller.socket_id, command, "stale terminal view revision");
            return;
        }
        let intent = intent_of(command);
        if command.revision == current.revision {
            // Renew BEFORE judging the intent: a same-revision command from the
            // owning socket proves liveness even when its geometry conflicts,
            // and dropping the renewal would expire the lease and park every
            // other session that socket was watching.
            current.deadline_ms = ViewRecord::lease_deadline(now_ms);
            let conflicting = !intents_equal(&current.intent, &intent);
            self.views.insert(current.key.clone(), current.clone());
            if conflicting {
                tracing::warn!(
                    session_id = %current.intent.session_id,
                    view_id = %current.view_id,
                    socket_id = %caller.socket_id,
                    revision = current.revision,
                    "terminal view revision conflict renewed"
                );
                outcome.refuse(
                    &caller.socket_id,
                    command,
                    "terminal view revision conflicts",
                );
                return;
            }
            outcome.answer_view(&current, TerminalViewStatus::Accepted, "");
            return;
        }
        if command.session_id != current.intent.session_id {
            outcome.refuse(&caller.socket_id, command, SESSION_MOVED);
            return;
        }
        if !command.active {
            let key = current.key.clone();
            self.release(&caller.socket_id, command, &key, now_ms, outcome);
            return;
        }
        let session_id = session_of(&current);
        let resized = current.intent.cols != command.cols || current.intent.rows != command.rows;
        current.intent = intent;
        current.revision = command.revision;
        current.deadline_ms = ViewRecord::lease_deadline(now_ms);
        self.views.insert(current.key.clone(), current.clone());
        if let Some(session_id) = session_id {
            outcome.changed.insert(session_id);
        }
        tracing::debug!(
            session_id = %current.intent.session_id,
            view_id = %current.view_id,
            cols = current.intent.cols,
            rows = current.intent.rows,
            resized,
            "terminal view resized"
        );
        outcome.answer_view(&current, TerminalViewStatus::Accepted, "");
    }

    /// A declaration for a handle this viewer key has never held, or whose
    /// retained claim has lapsed.
    fn admit(
        &mut self,
        caller: &Caller,
        command: &TerminalViewCommand,
        key: String,
        now_ms: u64,
        outcome: &mut MembershipOutcome,
    ) {
        let intent = intent_of(command);
        if let Some(old) = self.tombstones.get(&key).cloned() {
            if command.revision < old.revision
                || (command.revision == old.revision && !intents_equal(&old.intent, &intent))
            {
                outcome.refuse(
                    &caller.socket_id,
                    command,
                    "stale or conflicting terminal view revision",
                );
                return;
            }
            if old.intent.session_id != command.session_id {
                outcome.refuse(&caller.socket_id, command, SESSION_MOVED);
                return;
            }
            if command.revision == old.revision && !command.active {
                outcome.accept_inactive(&caller.socket_id, command);
                return;
            }
            self.tombstones.remove(&key);
        }
        if !command.active {
            self.tombstones.retain(
                now_ms,
                key,
                caller.viewer_key.clone(),
                command.revision,
                intent,
            );
            outcome.accept_inactive(&caller.socket_id, command);
            return;
        }
        if self.view_count(&caller.socket_id) >= TERMINAL_SOCKET_VIEW_CAP {
            outcome.refuse(
                &caller.socket_id,
                command,
                "terminal socket view capacity exceeded",
            );
            return;
        }
        let Ok(session_id) = SessionId::try_from(command.session_id.clone()) else {
            return;
        };
        if self.session_count(&session_id) >= SESSION_VIEW_CAP {
            outcome.refuse(
                &caller.socket_id,
                command,
                "terminal session view capacity exceeded",
            );
            return;
        }
        let record = ViewRecord {
            key: key.clone(),
            view_id: command.view_id.clone(),
            viewer_key: caller.viewer_key.clone(),
            fingerprint: caller.fingerprint.clone(),
            socket_id: caller.socket_id.clone(),
            intent,
            revision: command.revision,
            deadline_ms: ViewRecord::lease_deadline(now_ms),
            parked: false,
            parked_at_ms: 0,
            constrains: true,
        };
        self.views.insert(key.clone(), record.clone());
        self.session_views
            .entry(session_id.clone())
            .or_default()
            .insert(key.clone());
        if let Some(socket) = self.sockets.get_mut(&caller.socket_id) {
            socket.views.insert(key);
        }
        outcome.changed.insert(session_id.clone());
        outcome
            .calls
            .extend(self.sync_watching(&caller.socket_id, &session_id));
        outcome.answer_view(&record, TerminalViewStatus::Accepted, "");
    }

    /// Drop a record on an explicit inactive declaration, keeping the claim so
    /// the same tab can reclaim the handle at a new size.
    fn release(
        &mut self,
        socket_id: &str,
        command: &TerminalViewCommand,
        key: &str,
        now_ms: u64,
        outcome: &mut MembershipOutcome,
    ) {
        let session_id = self.views.get(key).and_then(session_of);
        self.drop_record(key, true, now_ms);
        if let Some(session_id) = session_id {
            outcome.changed.insert(session_id.clone());
            outcome.calls.extend(self.sync_watching(socket_id, &session_id));
        }
        outcome.accept_inactive(socket_id, command);
    }
}

impl MembershipOutcome {
    /// Refuse a command on the socket that sent it.
    fn refuse(&mut self, socket_id: &str, command: &TerminalViewCommand, reason: &str) {
        self.replies.push(PendingReply::Command {
            socket_id: socket_id.to_owned(),
            view_id: command.view_id.clone(),
            session_id: command.session_id.clone(),
            revision: command.revision,
            active: command.active,
            status: TerminalViewStatus::Rejected,
            reason: reason.to_owned(),
        });
    }

    /// Acknowledge a view the socket released, carrying no geometry: there is
    /// nothing left for it to paint.
    fn accept_inactive(&mut self, socket_id: &str, command: &TerminalViewCommand) {
        self.replies.push(PendingReply::Command {
            socket_id: socket_id.to_owned(),
            view_id: command.view_id.clone(),
            session_id: command.session_id.clone(),
            revision: command.revision,
            active: false,
            status: TerminalViewStatus::Accepted,
            reason: String::new(),
        });
    }
}
