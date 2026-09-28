//! The screen hub's boundary contracts: what a watching socket provides, what
//! the replica's owner is told, and the timer seam the repair ladder runs on.
//!
//! Ports `apps/coord/src/terminal/screen/terminal-screen-hub-contract.ts`. The
//! Sync socket implements the socket sink (`sync_ws::terminal::screen_socket`);
//! the view owner implements the replica sink (`terminal_view::screen_repair`).

use std::sync::Arc;
use std::time::Duration;

use roost_proto::FirehoseFrame;
use roost_protocol::wire::SessionId;

use crate::sync_ws::terminal::TerminalDeltaOutcome;
use crate::sync_ws::terminal::snapshot::TerminalSnapshotSource;

/// One watching socket, as the hub drives it.
pub trait TerminalScreenSocketSink: Send + Sync {
    /// Start, or restart, a session's stream on this socket; `false` when it
    /// already carries that stream.
    fn begin_terminal_stream(&self, session_id: &SessionId, stream_id: &str) -> bool;

    /// Install a canonical full for the session; `false` when the socket did
    /// not take it.
    fn replace_terminal_snapshot(
        &self,
        session_id: &SessionId,
        stream_id: &str,
        source: Arc<dyn TerminalSnapshotSource>,
    ) -> bool;

    /// Buffer one folded delta, or say what the hub owes the socket instead.
    fn enqueue_terminal_delta(
        &self,
        session_id: &SessionId,
        stream_id: &str,
        frame: &FirehoseFrame,
    ) -> TerminalDeltaOutcome;

    /// Forget the session's lane on this socket.
    fn drop_terminal_session(&self, session_id: &SessionId);
}

/// The boundary callbacks the replica owes its owner (v2's
/// `TerminalScreenHubOptions`).
pub trait ScreenReplicaSink: Send + Sync {
    /// The replica needs a source full for the stream it expects.
    fn request_snapshot(&self, session_id: &SessionId, stream_id: &str);
    /// Two snapshot requests produced nothing; only a fresh stream can repair.
    fn request_fresh_stream(&self, session_id: &SessionId, expected_stream_id: &str, reason: &str);
    /// The session's screen cannot be served, and the named reason is why.
    fn unavailable(&self, session_id: &SessionId, reason: &str);
    /// A full baseline was admitted and is now the session's current replica.
    fn full_accepted(&self, session_id: &SessionId, stream_id: &str);
}

/// A replica sink with nobody behind it: a coordinator built without a view
/// owner, which is a test. Every call is logged, because a repair request that
/// reached no worker is a screen that stays blank.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoScreenReplicaSink;

impl ScreenReplicaSink for NoScreenReplicaSink {
    fn request_snapshot(&self, session_id: &SessionId, stream_id: &str) {
        tracing::warn!(session_id = %session_id, stream_id, "a terminal snapshot request has no owner to reach");
    }

    fn request_fresh_stream(&self, session_id: &SessionId, expected_stream_id: &str, reason: &str) {
        tracing::warn!(session_id = %session_id, expected_stream_id, reason, "a fresh terminal stream request has no owner to reach");
    }

    fn unavailable(&self, session_id: &SessionId, reason: &str) {
        tracing::warn!(session_id = %session_id, reason, "a terminal screen became unavailable");
    }

    fn full_accepted(&self, session_id: &SessionId, stream_id: &str) {
        tracing::debug!(session_id = %session_id, stream_id, "a terminal baseline was accepted");
    }
}

/// The deadline seam: v2's injectable `setTimer`. A deadline is never
/// cancelled here; each one re-checks the token it was armed with and does
/// nothing when that token is no longer current.
pub trait ScreenTimers: Send + Sync {
    /// Run `fire` once, `delay_ms` from now.
    fn schedule(&self, delay_ms: u64, fire: Box<dyn FnOnce() + Send>);
}

/// Deadlines on the tokio runtime the coordinator serves from.
#[derive(Debug, Clone, Copy, Default)]
pub struct TokioScreenTimers;

impl ScreenTimers for TokioScreenTimers {
    fn schedule(&self, delay_ms: u64, fire: Box<dyn FnOnce() + Send>) {
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn(async move {
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    fire();
                });
            }
            // A hub driven outside a runtime is a synchronous test; the missed
            // deadline is named rather than silently lost.
            Err(_) => tracing::error!(
                delay_ms,
                "a terminal screen deadline could not be armed: no async runtime"
            ),
        }
    }
}
