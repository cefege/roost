//! Shared Sync socket synchronization and producer wakeups.
//! The socket drains LinkState; feed publishers wake that socket.
//! Durable recovery waits on separate progress so it cannot steal a flush wake.

use std::sync::{Mutex, MutexGuard, PoisonError};

use tokio::sync::Notify;
use tokio::sync::futures::Notified;

use super::driver::{LinkState, SyncLink, now_ms};
use super::feed::FeedFrame;

impl SyncLink {
    /// A link over freshly built state.
    #[must_use]
    pub fn new(state: LinkState) -> Self {
        Self {
            state: Mutex::new(state),
            wake: Notify::new(),
            recovery_progress: Notify::new(),
        }
    }

    /// The shared state; never held across an await or bus publication.
    pub fn lock(&self) -> MutexGuard<'_, LinkState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Route a bus message and wake the socket task after releasing its lock.
    pub fn deliver_with(&self, route: impl FnOnce(&mut LinkState) -> Option<FeedFrame>) {
        {
            let mut state = self.lock();
            if state.close.is_some() {
                return;
            }
            if let Some(frame) = route(&mut state) {
                state.deliver(frame, now_ms());
            }
        }
        self.wake_socket();
        self.signal_recovery_progress();
    }

    /// A listener handed the socket work; a wake during a write is retained.
    pub async fn woken(&self) {
        self.wake.notified().await;
    }

    pub(super) fn wake_socket(&self) {
        self.wake.notify_one();
    }

    pub(super) fn recovery_progress(&self) -> Notified<'_> {
        self.recovery_progress.notified()
    }

    pub(super) fn signal_recovery_progress(&self) {
        self.recovery_progress.notify_waiters();
    }
}
