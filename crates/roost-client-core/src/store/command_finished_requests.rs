//! Pending command completion events received from terminal output.
//!
//! The Sync fold owns admission and the browser scheduler drains each event once. The
//! queue is bounded because an offline UI must not retain unbounded command history.

use std::collections::VecDeque;

/// One command completion as reported by the terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandFinishedRequest {
    /// The session whose terminal completed the command.
    pub session_id: String,
    /// The exit status, or `None` when the command did not report one.
    pub exit_code: Option<i32>,
    /// Command duration in milliseconds.
    pub duration_ms: u64,
    /// The Sync sequence that delivered the event.
    pub delivery_seq: u64,
}

/// Pending command completions in arrival order, bounded to 64.
#[derive(Debug, Default)]
pub struct CommandFinishedRequests(VecDeque<CommandFinishedRequest>);

impl CommandFinishedRequests {
    /// Queue a completion, dropping the oldest at capacity.
    pub fn push(&mut self, request: CommandFinishedRequest) {
        if self.0.len() == 64 {
            self.0.pop_front();
            tracing::warn!(target: "notifications", "command completion queue full; oldest dropped");
        }
        self.0.push_back(request);
    }

    /// Remove all currently pending completions in arrival order.
    pub fn drain(&mut self) -> impl Iterator<Item = CommandFinishedRequest> + '_ {
        self.0.drain(..)
    }

    /// Discard completions that belong to a credential no longer active.
    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// Pending completions in arrival order.
    pub fn iter(&self) -> impl Iterator<Item = &CommandFinishedRequest> {
        self.0.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::{CommandFinishedRequest, CommandFinishedRequests};

    #[test]
    fn queue_retains_arrival_order_and_caps_pending_events() {
        let mut requests = CommandFinishedRequests::default();
        for delivery_seq in 1..=65 {
            requests.push(CommandFinishedRequest {
                session_id: format!("session-{delivery_seq}"),
                exit_code: Some(0),
                duration_ms: 10,
                delivery_seq,
            });
        }
        let held = requests
            .iter()
            .map(|request| request.delivery_seq)
            .collect::<Vec<_>>();
        assert_eq!(held, (2..=65).collect::<Vec<_>>());
        assert_eq!(requests.drain().count(), 64);
    }
}
