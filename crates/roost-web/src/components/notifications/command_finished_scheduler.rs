//! Debounce and admission rules for terminal command-completion notifications.

use std::collections::BTreeMap;

/// The one-second quiet period before a command completion is delivered.
pub const COMMAND_FINISHED_DELAY_MS: u64 = 1_000;

/// One pending timer, tied to a specific observed completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArmedCommandFinished {
    /// Session whose completion is pending.
    pub session_id: String,
    /// Ticket which invalidates replaced or cancelled timers.
    pub ticket: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Pending {
    ticket: u64,
}

/// One pending completion per session.
#[derive(Debug, Default)]
pub struct CommandFinishedScheduler {
    pending: BTreeMap<String, Pending>,
    last_ticket: u64,
}

impl CommandFinishedScheduler {
    /// Start or replace a completion timer unless the session is suppressed.
    pub fn observe(&mut self, session_id: &str, suppressed: bool) -> Option<ArmedCommandFinished> {
        self.cancel(session_id);
        if suppressed {
            return None;
        }
        self.last_ticket = self.last_ticket.wrapping_add(1);
        let ticket = self.last_ticket;
        self.pending
            .insert(session_id.to_owned(), Pending { ticket });
        Some(ArmedCommandFinished {
            session_id: session_id.to_owned(),
            ticket,
        })
    }

    /// Claim the completion if this timer still owns it.
    pub fn take_due(&mut self, armed: &ArmedCommandFinished) -> bool {
        if self
            .pending
            .get(&armed.session_id)
            .map(|pending| pending.ticket)
            != Some(armed.ticket)
        {
            return false;
        }
        self.pending.remove(&armed.session_id);
        true
    }

    /// Cancel a pending timer for this session.
    pub fn cancel(&mut self, session_id: &str) {
        self.pending.remove(session_id);
    }
}

#[cfg(test)]
mod tests {
    use super::{ArmedCommandFinished, CommandFinishedScheduler};

    #[test]
    fn replacement_and_suppression_invalidate_older_timers() {
        let mut scheduler = CommandFinishedScheduler::default();
        let first = ArmedCommandFinished {
            session_id: "session".to_owned(),
            ticket: 1,
        };
        let second = ArmedCommandFinished {
            session_id: "session".to_owned(),
            ticket: 2,
        };
        assert!(scheduler.observe("session", false).is_some());
        assert!(scheduler.observe("session", false).is_some());
        assert!(!scheduler.take_due(&first));
        assert!(scheduler.take_due(&second));

        let third = ArmedCommandFinished {
            session_id: "session".to_owned(),
            ticket: 3,
        };
        assert!(scheduler.observe("session", false).is_some());
        assert!(scheduler.observe("session", true).is_none());
        assert!(!scheduler.take_due(&third));
    }
}
