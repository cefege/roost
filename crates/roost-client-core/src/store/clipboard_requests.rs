//! Pending one-shot clipboard writes received from terminal output.
//!
//! The Sync fold owns admission and the web shell drains requests after each
//! store revision. Requests are bounded because clipboard permission may be
//! unavailable while a terminal emits an arbitrary amount of OSC 52 traffic.

use std::collections::VecDeque;

/// One OSC 52 write request, identified by its Sync delivery sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalClipboardRequest {
    /// The session whose visible terminal emitted the request.
    pub session_id: String,
    /// The decoded clipboard text.
    pub text: String,
    /// The Sync sequence that delivered the request.
    pub delivery_seq: u64,
}

/// The newest pending clipboard request for up to eight sessions.
#[derive(Debug, Default)]
pub struct TerminalClipboardRequests(VecDeque<TerminalClipboardRequest>);

impl TerminalClipboardRequests {
    /// Keep only the latest request for each session, evicting the oldest session at capacity.
    pub fn push(&mut self, request: TerminalClipboardRequest) {
        self.0
            .retain(|pending| pending.session_id != request.session_id);
        if self.0.len() == 8 {
            self.0.pop_front();
            tracing::warn!(
                target: "terminal",
                "clipboard request session cap reached; oldest session dropped"
            );
        }
        self.0.push_back(request);
    }

    /// Remove every pending request in arrival order.
    pub fn drain(&mut self) -> impl Iterator<Item = TerminalClipboardRequest> + '_ {
        self.0.drain(..)
    }

    /// Discard requests owned by a credential that is no longer active.
    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// The pending requests in arrival order.
    pub fn iter(&self) -> impl Iterator<Item = &TerminalClipboardRequest> {
        self.0.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::{TerminalClipboardRequest, TerminalClipboardRequests};

    #[test]
    fn queue_keeps_the_newest_request_per_session_and_eight_sessions() {
        let mut requests = TerminalClipboardRequests::default();
        for delivery_seq in 1..=10 {
            requests.push(TerminalClipboardRequest {
                session_id: format!("session-{delivery_seq}"),
                text: delivery_seq.to_string(),
                delivery_seq,
            });
        }
        requests.push(TerminalClipboardRequest {
            session_id: "session-10".to_owned(),
            text: "latest".to_owned(),
            delivery_seq: 11,
        });
        let held = requests
            .iter()
            .map(|request| request.delivery_seq)
            .collect::<Vec<_>>();
        assert_eq!(held, (3..=9).chain([11]).collect::<Vec<_>>());
        let drained = requests.drain().collect::<Vec<_>>();
        assert_eq!(
            drained
                .iter()
                .map(|request| request.delivery_seq)
                .collect::<Vec<_>>(),
            (3..=9).chain([11]).collect::<Vec<_>>()
        );
        assert!(requests.iter().next().is_none());
    }
}
