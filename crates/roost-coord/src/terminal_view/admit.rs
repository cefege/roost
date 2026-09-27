//! Admission: a declaration for a handle this viewer key has never held, or
//! whose retained claim has lapsed.
//!
//! Split out of `commands.rs` for the size cap, and split by CONCEPT rather than
//! by convenience: the paths that decide a record's EXISTENCE live here, and
//! the paths that change one that already exists live there.

use roost_proto::{TerminalViewCommand, TerminalViewStatus};
use roost_protocol::viewport::TERMINAL_SOCKET_VIEW_CAP;
use roost_protocol::wire::SessionId;

use super::commands::{Caller, SESSION_MOVED};
use super::machine::Machine;
use super::record::{SESSION_VIEW_CAP, ViewRecord, intent_of, intents_equal};
use super::registry::MembershipOutcome;

impl Machine<'_> {
    /// A declaration for a handle this viewer key has never held, or whose
    /// retained claim has lapsed.
    ///
    /// `pub(super)` because the command dispatcher in the sibling `commands.rs`
    /// is its only caller, and that module is inside `terminal_view`.
    pub(super) fn admit(
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
}
