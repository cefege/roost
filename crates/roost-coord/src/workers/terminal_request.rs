//! One typed terminal-control request to a worker: whether its frame reached
//! the socket, whether its budget expired before it could, and the bounded wait
//! for the worker's typed result.
//! Ports `TerminalWorkerRequest` and `unsentTerminalWorkerRequest` of
//! `apps/coord/src/workers/worker-send.ts:152-172`. Built by `workers::terminal_send`
//! and the route-claim sender; awaited by the input, stream and route lanes.

use std::marker::PhantomData;
use std::time::Duration;

use connectrpc::{ConnectError, ErrorCode};
use tokio::time::Instant;

use crate::terminal_screen::pending_rpcs::PendingRpc;
use crate::terminal_screen::typed_results::TypedResult;
use crate::workers::hop_deadline::HopDeadline;

/// One typed request, sent or not.
///
/// ADMISSION IS NOT COMPLETION. `is_admitted` says only that the socket took
/// the frame; the verdict arrives exclusively as the worker's typed result, a
/// rejection, or the deadline. A caller that reads admission as success is
/// fabricating a keeper write nobody proved.
#[derive(Debug)]
pub struct TerminalWorkerRequest<T> {
    admitted: bool,
    expired: bool,
    state: RequestState,
    result: PhantomData<fn() -> T>,
}

#[derive(Debug)]
enum RequestState {
    /// Nothing was written; the result is this refusal.
    Unsent(ConnectError),
    /// A correlation entry is open and waits until `wait_until`.
    Pending {
        pending: PendingRpc,
        wait_until: Instant,
        timeout_ms: u64,
    },
}

impl<T: TypedResult> TerminalWorkerRequest<T> {
    /// A definite pre-write failure: nothing was written, so a caller may
    /// reject definitely and a retry cannot duplicate. `expired` says the hop
    /// budget ran out before the frame could reach the socket. The result is
    /// `Unavailable` with `reason`, as v2's `unsentTerminalWorkerRequest`.
    #[must_use]
    pub fn unsent(reason: &str, expired: bool) -> Self {
        Self::refused(ConnectError::new(ErrorCode::Unavailable, reason), expired)
    }

    /// A request that could not open its correlation entry, so nothing was
    /// written; the result is that error unchanged.
    #[must_use]
    pub fn uncorrelated(error: ConnectError) -> Self {
        Self::refused(error, false)
    }

    /// A request whose correlation entry is open.
    ///
    /// `admitted` is whether the socket took the frame. A sender that saw the
    /// write refused has already settled `pending` as `Unavailable`, so the
    /// result reports that at once. The wait is v2's pending timer: at least
    /// one millisecond and otherwise what the hop deadline has left, measured
    /// NOW, so a caller that awaits the result later does not extend it.
    #[must_use]
    pub fn from_pending(pending: PendingRpc, deadline: &HopDeadline, admitted: bool) -> Self {
        let timeout_ms = deadline.remaining_ms().ceil().max(1.0) as u64;
        Self {
            admitted,
            expired: false,
            state: RequestState::Pending {
                pending,
                wait_until: Instant::now() + Duration::from_millis(timeout_ms),
                timeout_ms,
            },
            result: PhantomData,
        }
    }

    /// Whether the socket accepted the frame.
    #[must_use]
    pub fn is_admitted(&self) -> bool {
        self.admitted
    }

    /// Whether the hop budget ran out before the frame reached the socket.
    /// Never true once admitted.
    #[must_use]
    pub fn is_expired(&self) -> bool {
        self.expired
    }

    /// The correlation id the typed result must echo; `None` before one was
    /// allocated.
    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        match &self.state {
            RequestState::Unsent(_) => None,
            RequestState::Pending { pending, .. } => Some(pending.request_id()),
        }
    }

    /// Wait for the worker's typed result.
    ///
    /// Settles exclusively from the typed result frame, a rejection (transport
    /// drop, worker close), or the deadline, which is `DeadlineExceeded` in
    /// v2's `pending-rpcs` wording: an expiry after admission is ambiguity,
    /// never a rejection the caller may retry as unsent.
    pub async fn result(self) -> Result<T, ConnectError> {
        let (mut pending, wait_until, timeout_ms) = match self.state {
            RequestState::Unsent(error) => return Err(error),
            RequestState::Pending {
                pending,
                wait_until,
                timeout_ms,
            } => (pending, wait_until, timeout_ms),
        };
        match tokio::time::timeout_at(wait_until, pending.settle_typed::<T>()).await {
            Ok(settled) => settled,
            Err(_) => {
                tracing::warn!(
                    request_id = pending.request_id(),
                    worker_fp = pending.worker_fp(),
                    timeout_ms,
                    kind = T::KIND,
                    "a worker did not reply before the terminal hop deadline"
                );
                Err(ConnectError::new(
                    ErrorCode::DeadlineExceeded,
                    format!("worker did not reply within {timeout_ms}ms"),
                ))
            }
        }
    }

    fn refused(error: ConnectError, expired: bool) -> Self {
        Self {
            admitted: false,
            expired,
            state: RequestState::Unsent(error),
            result: PhantomData,
        }
    }
}
