//! Coordinator-owned retained terminal signals: each live session's OSC 9;4
//! progress report, OSC 1337 user variables, and OSC 7501 program status
//! records, deduplicated and fanned out on `terminal_signal_bus`; desktop
//! notifications pass straight through.
//! `worker_link::live_frames::publish_metadata` observes here, the Sync seed
//! reads the snapshot, and a closed session drops its progress and user
//! variables while keeping its `done` and `error` program status records.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use roost_protocol::program_status::{ProgramStatusRecord, ProgramStatusTable};
use roost_protocol::terminal_signals::{TerminalNotification, TerminalProgress, TerminalUserVar};

use crate::events::bus::Subscription;
use crate::events::bus_domains::Buses;
use crate::events::bus_messages::{SessionBusMessage, SessionTerminalSignals};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Retained {
    progress: Option<TerminalProgress>,
    user_vars: Vec<TerminalUserVar>,
    program_status: Vec<ProgramStatusRecord>,
}

impl Retained {
    fn is_empty(&self) -> bool {
        self.progress.is_none() && self.user_vars.is_empty() && self.program_status.is_empty()
    }
}

/// Every live session's retained progress and user variables.
#[derive(Debug, Default)]
pub struct TerminalSignalHub {
    entries: Mutex<BTreeMap<String, Retained>>,
}

impl TerminalSignalHub {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Accept one worker observation and publish what changed. `progress`
    /// `Clear` forgets the report; an empty `user_vars` forgets the variables.
    pub fn observe(
        &self,
        buses: &Buses,
        session_id: &str,
        progress: Option<TerminalProgress>,
        user_vars: Option<Vec<TerminalUserVar>>,
        notification: Option<TerminalNotification>,
    ) {
        let (progress, user_vars) = {
            let mut entries = self.lock();
            let entry = entries.entry(session_id.to_owned()).or_default();
            let progress = progress.filter(|progress| {
                let next = (*progress != TerminalProgress::Clear).then_some(*progress);
                let changed = next != entry.progress;
                entry.progress = next;
                changed
            });
            let user_vars = user_vars.filter(|vars| {
                let changed = *vars != entry.user_vars;
                entry.user_vars.clone_from(vars);
                changed
            });
            if entry.is_empty() {
                entries.remove(session_id);
            }
            (progress, user_vars)
        };
        if progress.is_none() && user_vars.is_none() && notification.is_none() {
            return;
        }
        tracing::debug!(
            event = "terminal_signals.change",
            session_id,
            progress = ?progress,
            user_vars = user_vars.as_ref().map(Vec::len),
            notification = notification.is_some(),
            "a session's terminal signals changed"
        );
        // Published with the map released: a bus listener is a Sync socket.
        buses.terminal_signal_bus.publish(SessionTerminalSignals {
            session_id: session_id.to_owned(),
            progress,
            user_vars,
            notification,
            program_status: None,
        });
    }

    /// Accept a worker's full OSC 7501 record set for `session_id`. The set is
    /// replaced, not merged, so an empty set clears the session's records.
    pub fn observe_program_status(
        &self,
        buses: &Buses,
        session_id: &str,
        records: Vec<ProgramStatusRecord>,
    ) {
        let changed = {
            let mut entries = self.lock();
            let entry = entries.entry(session_id.to_owned()).or_default();
            let changed = entry.program_status != records;
            if changed {
                entry.program_status.clone_from(&records);
            }
            if entry.is_empty() {
                entries.remove(session_id);
            }
            changed
        };
        if !changed {
            return;
        }
        tracing::debug!(
            event = "terminal_signals.program_status",
            session_id,
            records = records.len(),
            "a session's program status records changed"
        );
        // Published with the map released: a bus listener is a Sync socket.
        buses.terminal_signal_bus.publish(SessionTerminalSignals {
            session_id: session_id.to_owned(),
            progress: None,
            user_vars: None,
            notification: None,
            program_status: Some(records),
        });
    }

    /// The retained signals of every session that has any, for a fresh Sync
    /// subscriber: the bus is publish-on-change and never backfilled.
    #[must_use]
    pub fn snapshot(&self) -> Vec<SessionTerminalSignals> {
        self.lock()
            .iter()
            .map(|(session_id, retained)| SessionTerminalSignals {
                session_id: session_id.clone(),
                progress: retained.progress,
                user_vars: (!retained.user_vars.is_empty()).then(|| retained.user_vars.clone()),
                notification: None,
                program_status: (!retained.program_status.is_empty())
                    .then(|| retained.program_status.clone()),
            })
            .collect()
    }

    /// A closed session drops its progress and user variables. Its program
    /// status keeps only the records the program left for the user; the rest
    /// go with the process. Publishes the surviving set when it changed, so a
    /// connected client sees the working record disappear.
    pub fn session_closed(&self, buses: &Buses, session_id: &str) {
        let surviving = {
            let mut entries = self.lock();
            let Some(entry) = entries.get_mut(session_id) else {
                return;
            };
            entry.progress = None;
            entry.user_vars.clear();
            let surviving = ProgramStatusTable::from_records(&entry.program_status)
                .retained_past_exit()
                .records();
            let changed = surviving != entry.program_status;
            entry.program_status.clone_from(&surviving);
            if entry.is_empty() {
                entries.remove(session_id);
            }
            changed.then_some(surviving)
        };
        let Some(records) = surviving else {
            return;
        };
        tracing::debug!(
            event = "terminal_signals.program_status",
            session_id,
            records = records.len(),
            "a closed session kept only its done and error program status records"
        );
        // Published with the map released: a bus listener is a Sync socket.
        buses.terminal_signal_bus.publish(SessionTerminalSignals {
            session_id: session_id.to_owned(),
            progress: None,
            user_vars: None,
            notification: None,
            program_status: Some(records),
        });
    }

    /// Apply the close rule to every closed session for as long as the
    /// returned handle lives.
    pub fn subscribe_session_close(
        self: &Arc<Self>,
        buses: &Buses,
    ) -> Subscription<SessionBusMessage> {
        let hub = Arc::clone(self);
        let publish_buses = buses.clone();
        buses.session_bus.subscribe(move |message| {
            if message.event.kind_name() != "closed" {
                return;
            }
            if let Some(session_id) = message.event.session_id() {
                hub.session_closed(&publish_buses, session_id.as_str());
            }
        })
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, Retained>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
