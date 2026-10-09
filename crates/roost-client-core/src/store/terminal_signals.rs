//! A program's terminal signals as the browser holds them: each session's
//! progress report and shell user variables (retained, replaced per frame) and
//! the desktop notifications not yet presented (a bounded queue the
//! notification presenter drains). The Sync fold (`handle_sync::apply_frame`)
//! writes here; the sidebar and the notification dock read.

use std::collections::{BTreeMap, VecDeque};

use roost_protocol::terminal_signals::{TerminalNotification, TerminalProgress, TerminalUserVar};

/// Notifications held for a presenter that has not drained them.
const PENDING_NOTIFICATIONS_CAP: usize = 32;

/// One program notification waiting to be shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalNotificationRequest {
    pub session_id: String,
    pub notification: TerminalNotification,
    /// The Sync sequence that delivered it.
    pub delivery_seq: u64,
}

#[derive(Debug, Default)]
pub struct TerminalSignals {
    progress: BTreeMap<String, TerminalProgress>,
    user_vars: BTreeMap<String, Vec<TerminalUserVar>>,
    notifications: VecDeque<TerminalNotificationRequest>,
}

impl TerminalSignals {
    /// Fold one frame's changed parts. `Clear` and an empty variable set
    /// remove the session's entry.
    pub fn apply(
        &mut self,
        session_id: &str,
        progress: Option<TerminalProgress>,
        user_vars: Option<&[TerminalUserVar]>,
        notification: Option<&TerminalNotification>,
        delivery_seq: u64,
    ) {
        match progress {
            Some(TerminalProgress::Clear) => {
                self.progress.remove(session_id);
            }
            Some(progress) => {
                self.progress.insert(session_id.to_owned(), progress);
            }
            None => {}
        }
        match user_vars {
            Some([]) => {
                self.user_vars.remove(session_id);
            }
            Some(vars) => {
                self.user_vars.insert(session_id.to_owned(), vars.to_vec());
            }
            None => {}
        }
        if let Some(notification) = notification {
            if self.notifications.len() == PENDING_NOTIFICATIONS_CAP {
                self.notifications.pop_front();
                tracing::warn!(target: "notifications", "terminal notification queue full; oldest dropped");
            }
            self.notifications.push_back(TerminalNotificationRequest {
                session_id: session_id.to_owned(),
                notification: notification.clone(),
                delivery_seq,
            });
        }
    }

    /// The session's progress report, if it shows one.
    pub fn progress(&self, session_id: &str) -> Option<TerminalProgress> {
        self.progress.get(session_id).copied()
    }

    /// The session's published shell variables, sorted by name.
    pub fn user_vars(&self, session_id: &str) -> &[TerminalUserVar] {
        self.user_vars.get(session_id).map_or(&[], Vec::as_slice)
    }

    /// Remove every pending notification in arrival order.
    pub fn drain_notifications(
        &mut self,
    ) -> impl Iterator<Item = TerminalNotificationRequest> + '_ {
        self.notifications.drain(..)
    }

    /// Forget the retained state a terminal reset's seed replaces.
    pub fn reset_retained(&mut self) {
        self.progress.clear();
        self.user_vars.clear();
    }

    /// Forget everything: a credential change.
    pub fn clear(&mut self) {
        self.reset_retained();
        self.notifications.clear();
    }
}

#[cfg(test)]
mod tests {
    use roost_protocol::terminal_signals::{
        TerminalNotification, TerminalProgress, TerminalUserVar,
    };

    use super::TerminalSignals;

    #[test]
    fn clear_and_empty_sets_remove_and_notifications_queue_in_order() {
        let mut signals = TerminalSignals::default();
        let vars = [TerminalUserVar {
            key: "branch".to_owned(),
            value: "main".to_owned(),
        }];
        signals.apply(
            "s",
            Some(TerminalProgress::Normal(40)),
            Some(&vars),
            None,
            1,
        );
        assert_eq!(signals.progress("s"), Some(TerminalProgress::Normal(40)));
        assert_eq!(signals.user_vars("s"), &vars);

        signals.apply("s", None, None, None, 2);
        assert_eq!(
            signals.progress("s"),
            Some(TerminalProgress::Normal(40)),
            "absent is unchanged"
        );

        signals.apply("s", Some(TerminalProgress::Clear), Some(&[]), None, 3);
        assert_eq!(signals.progress("s"), None);
        assert!(signals.user_vars("s").is_empty());

        let done = TerminalNotification {
            title: String::new(),
            body: "Done".to_owned(),
        };
        signals.apply("s", None, None, Some(&done), 4);
        let drained: Vec<_> = signals.drain_notifications().collect();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].notification, done);
        assert_eq!(signals.drain_notifications().count(), 0);
    }
}
