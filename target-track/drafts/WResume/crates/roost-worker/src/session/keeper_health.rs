//! Whether the keeper is still handing out working PTYs, as the session layer
//! observes it: output for channels with no record (`emit_no_session`), the
//! post-close tail window that excuses it, and the degraded hook both it and
//! the dead-birth burst fire. Ports `apps/worker/src/session/session-emit.ts:88-113`,
//! `session-lifecycle.ts` `markRecentlyClosed`, and `setOnKeeperDegraded`
//! (`session-manager-state.ts`). `SessionManager` owns one; `binding_close`
//! and `respawn::note_birth` report into it; `runtime::reconcile_gate` hooks it.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::strays::{DEGRADED_WINDOW, RECENTLY_CLOSED_TTL};

/// Orphan chunks inside [`DEGRADED_WINDOW`] that mark a degraded keeper (v2
/// `KEEPER_DEGRADED_THRESHOLD`).
pub const KEEPER_DEGRADED_THRESHOLD: usize = 5;

/// What the degraded signal calls: the reconcile gate's remediation.
pub type KeeperDegradedHook = Arc<dyn Fn() + Send + Sync>;

/// The keeper-health facts one worker keeps.
#[derive(Default)]
pub struct KeeperHealth {
    recently_closed: Mutex<HashMap<u16, i64>>,
    no_session_burst: Mutex<VecDeque<i64>>,
    hook: Mutex<Option<KeeperDegradedHook>>,
}

impl std::fmt::Debug for KeeperHealth {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KeeperHealth")
            .field("recently_closed", &lock(&self.recently_closed).len())
            .field("no_session_burst", &lock(&self.no_session_burst).len())
            .finish_non_exhaustive()
    }
}

impl KeeperHealth {
    /// Register the one degraded hook (v2 `setOnKeeperDegraded`).
    pub fn set_hook(&self, hook: KeeperDegradedHook) {
        *lock(&self.hook) = Some(hook);
    }

    /// A channel whose record was just torn down: its in-flight keeper frames
    /// are a benign tail for [`RECENTLY_CLOSED_TTL`], not a degraded keeper.
    pub fn mark_recently_closed(&self, channel_id: u16, now_ms: i64) {
        let ttl = RECENTLY_CLOSED_TTL.as_millis() as i64;
        let mut closed = lock(&self.recently_closed);
        closed.insert(channel_id, now_ms);
        closed.retain(|_, at| now_ms.saturating_sub(*at) < ttl);
    }

    /// Whether `channel_id` was torn down inside the tail window.
    pub fn is_recently_closed(&self, channel_id: u16, now_ms: i64) -> bool {
        let ttl = RECENTLY_CLOSED_TTL.as_millis() as i64;
        lock(&self.recently_closed)
            .get(&channel_id)
            .is_some_and(|at| now_ms.saturating_sub(*at) < ttl)
    }

    /// Output for a channel with no record. Inside the tail window it is
    /// dropped silently; past it, it counts toward the burst, and a full burst
    /// fires the degraded hook. Returns whether it counted.
    pub fn note_orphan_output(&self, channel_id: u16, len: usize, now_ms: i64) -> bool {
        if self.is_recently_closed(channel_id, now_ms) {
            tracing::debug!(channel_id, len, "session.tail_drop: a closed channel's in-flight output was dropped");
            return false;
        }
        tracing::warn!(channel_id, len, "session-manager: emit_no_session");
        let burst = {
            let mut burst = lock(&self.no_session_burst);
            burst.push_back(now_ms);
            let window = DEGRADED_WINDOW.as_millis() as i64;
            while burst.front().is_some_and(|at| *at < now_ms - window) {
                burst.pop_front();
            }
            burst.len()
        };
        if burst >= KEEPER_DEGRADED_THRESHOLD {
            tracing::error!(
                no_session_count = burst,
                window_ms = DEGRADED_WINDOW.as_millis() as u64,
                "keeper.degraded: the keeper is emitting on channels with no session"
            );
            self.degraded();
        }
        true
    }

    /// Fire the degraded hook, from either detector.
    pub fn degraded(&self) {
        let hook = lock(&self.hook).clone();
        match hook {
            Some(hook) => hook(),
            None => tracing::warn!("keeper.degraded with no remediation registered"),
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
