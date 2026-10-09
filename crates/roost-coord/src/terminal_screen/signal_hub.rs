//! Coordinator-owned retained terminal signals: each live session's OSC 9;4
//! progress report and OSC 1337 user variables, deduplicated and fanned out on
//! `terminal_signal_bus`; desktop notifications pass straight through.
//! `worker_link::live_frames::publish_metadata` observes here, the Sync seed
//! reads the snapshot, and a closed session releases its entry.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use roost_protocol::terminal_signals::{TerminalNotification, TerminalProgress, TerminalUserVar};

use crate::events::bus::Subscription;
use crate::events::bus_domains::Buses;
use crate::events::bus_messages::{SessionBusMessage, SessionTerminalSignals};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Retained {
    progress: Option<TerminalProgress>,
    user_vars: Vec<TerminalUserVar>,
}

impl Retained {
    fn is_empty(&self) -> bool {
        self.progress.is_none() && self.user_vars.is_empty()
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
            })
            .collect()
    }

    /// Forget a closed session, so a reused id starts without signals.
    pub fn release(&self, session_id: &str) {
        if self.lock().remove(session_id).is_some() {
            tracing::debug!(
                event = "terminal_signals.released",
                session_id,
                "a closed session released its retained terminal signals"
            );
        }
    }

    /// Release every closed session for as long as the returned handle lives.
    pub fn subscribe_session_close(
        self: &Arc<Self>,
        buses: &Buses,
    ) -> Subscription<SessionBusMessage> {
        let hub = Arc::clone(self);
        buses.session_bus.subscribe(move |message| {
            if message.event.kind_name() != "closed" {
                return;
            }
            if let Some(session_id) = message.event.session_id() {
                hub.release(session_id.as_str());
            }
        })
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, Retained>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
