//! How a coordinator stops: the signal that asks it to, the readiness flag that
//! turns new traffic away, and the bound on how long open connections may hold
//! the exit.
//!
//! Called by `serve::serve`; the flag is the one `/readyz` reads
//! (`http::health`). Unbounded, a single long-lived WebSocket would hold a
//! terminating container until its orchestrator's SIGKILL, which skips every
//! step after the listener. Keepers and workers redial on their own, so ending
//! with sockets open loses nothing they cannot rebuild.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::Notify;

/// How long open connections may delay exit once shutdown is requested. Under
/// a Kubernetes pod's default 30 s grace period, so the bounded exit — and the
/// steps after it — run before the kubelet's SIGKILL.
pub const SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(20);

/// The drain for one listener: raises the readiness flag when shutdown is
/// requested, then bounds how long the server may take to finish.
#[derive(Debug, Clone)]
pub struct ShutdownDrain {
    draining: Arc<AtomicBool>,
    requested: Arc<Notify>,
    timeout: Duration,
}

impl ShutdownDrain {
    /// A drain that raises `draining` and allows `timeout` after the request.
    #[must_use]
    pub fn new(draining: Arc<AtomicBool>, timeout: Duration) -> Self {
        Self {
            draining,
            requested: Arc::new(Notify::new()),
            timeout,
        }
    }

    /// The future the server's graceful shutdown waits on: `signal`, then the
    /// readiness flag, then the start of the drain clock.
    pub fn requested<S>(&self, signal: S) -> impl Future<Output = ()> + use<S>
    where
        S: Future<Output = ()>,
    {
        let draining = Arc::clone(&self.draining);
        let requested = Arc::clone(&self.requested);
        async move {
            signal.await;
            draining.store(true, Ordering::Release);
            tracing::info!("coordinator draining: readiness withdrawn, listener closed");
            // `notify_one` stores a permit, so the bound below starts its clock
            // even if it has not begun waiting yet.
            requested.notify_one();
        }
    }

    /// Run `served` to completion, or until the drain timeout has passed since
    /// shutdown was requested. `None` means the bound cut it short.
    pub async fn bound<T>(&self, served: impl Future<Output = T>) -> Option<T> {
        let expired = async {
            self.requested.notified().await;
            tokio::time::sleep(self.timeout).await;
        };
        tokio::select! {
            outcome = served => Some(outcome),
            () = expired => {
                tracing::warn!(
                    timeout_secs = self.timeout.as_secs(),
                    "coordinator shutdown drain timed out; exiting with connections open"
                );
                None
            }
        }
    }
}

/// The platform's termination signal.
///
/// `SIGTERM` **and** `SIGINT` are both wired, because systemd and Kubernetes
/// send the first and a terminal sends the second, and a coordinator that only
/// handled one of them would need a `SIGKILL` to stop -- which is the one path
/// that does not run the shutdown sequence.
pub async fn shutdown_signal() {
    let interrupt = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        {
            signal.recv().await;
        }
    };
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    tokio::select! {
        () = interrupt => {},
        () = terminate => {},
    }
    tracing::info!("coordinator shutdown");
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::ShutdownDrain;

    #[tokio::test(start_paused = true)]
    async fn a_server_that_never_finishes_is_cut_off_after_the_drain_timeout() {
        let draining = Arc::new(AtomicBool::new(false));
        let drain = ShutdownDrain::new(Arc::clone(&draining), Duration::from_secs(20));
        let (fire, signal) = tokio::sync::oneshot::channel::<()>();
        let shutdown = drain.requested(async {
            let _ = signal.await;
        });
        let stuck = async {
            shutdown.await;
            std::future::pending::<()>().await;
        };
        let started = tokio::time::Instant::now();
        let bounded = tokio::spawn({
            let drain = drain.clone();
            async move { drain.bound(stuck).await }
        });
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert!(
            !draining.load(Ordering::Acquire),
            "ready until asked to stop"
        );
        let _ = fire.send(());
        let outcome = bounded.await.unwrap_or(Some(()));
        assert_eq!(outcome, None, "the bound, not the server, ended the wait");
        assert!(
            draining.load(Ordering::Acquire),
            "readiness withdrawn on request"
        );
        assert_eq!(started.elapsed(), Duration::from_secs(25));
    }

    #[tokio::test(start_paused = true)]
    async fn a_server_that_finishes_inside_the_bound_returns_its_outcome() {
        let drain = ShutdownDrain::new(Arc::default(), Duration::from_secs(20));
        let shutdown = drain.requested(std::future::ready(()));
        let served = async {
            shutdown.await;
            tokio::time::sleep(Duration::from_secs(3)).await;
            7
        };
        assert_eq!(drain.bound(served).await, Some(7));
    }
}
