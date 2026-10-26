//! Harness utilities: keyed waiters and paginated scans
//! (port of `packages/durable/src/harness/util.ts` @ `abe508e1b`).
//!
//! Upstream `Waiters<K, T>` stores `Promise.withResolvers` entries and detaches
//! them when their context aborts. The Rust port stores `tokio::sync::oneshot`
//! senders in a mutex-guarded map, cancels through the Chord context's
//! cancellation signal, and removes a waiter when its future is dropped.

use std::collections::HashMap;
use std::future::Future;
use std::hash::Hash;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use thiserror::Error;
use tokio::sync::oneshot;

use crate::chord::context::Context;
use crate::session::{SessionError, SessionResult};
use crate::types::{Cursor, Page};

/// Failure of one waiter: caller cancellation, harness close, or session failure.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum WaitError {
    /// The caller's context was cancelled.
    #[error("The operation was aborted")]
    Aborted,
    /// The harness closed while the wait was pending.
    #[error("Harness is closed")]
    Closed,
    /// Storage failure terminated the owning session.
    #[error("Session failed: {0}")]
    SessionFailed(String),
}

/// The error every pending wait rejects with when the harness closes.
pub fn closed_error() -> WaitError {
    WaitError::Closed
}

/// A boxed waiter future (upstream `Promise<T>`).
pub type WaitFuture<T> = Pin<Box<dyn Future<Output = Result<T, WaitError>> + Send + 'static>>;

struct Waiter<T> {
    id: u64,
    sender: oneshot::Sender<Result<T, WaitError>>,
}

struct Inner<K, T> {
    state: Mutex<HashMap<K, Vec<Waiter<T>>>>,
    next_id: AtomicU64,
}

/// Pending waits by key. Each settles once: through `resolve`, `reject_all`, or
/// cancellation of its context.
pub struct Waiters<K, T> {
    inner: Arc<Inner<K, T>>,
}

impl<K, T> Clone for Waiters<K, T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<K, T> Default for Waiters<K, T> {
    fn default() -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
            }),
        }
    }
}

impl<K, T> Waiters<K, T>
where
    K: Eq + Hash + Clone + Send + 'static,
    T: Clone + Send + 'static,
{
    /// Create an empty waiter set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a waiter for `key`; the returned future resolves with the value
    /// passed to [`Waiters::resolve`], or rejects on context cancellation.
    pub fn add(&self, key: K, context: &Context) -> WaitFuture<T> {
        let signal = context.abort_signal();
        if signal.as_ref().is_some_and(|signal| signal.is_cancelled()) {
            return Box::pin(async { Err(WaitError::Aborted) });
        }
        let (sender, receiver) = oneshot::channel();
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut state) = self.inner.state.lock() {
            state
                .entry(key.clone())
                .or_default()
                .push(Waiter { id, sender });
        }
        let inner = Arc::clone(&self.inner);
        let cleanup_key = key.clone();
        let registration = Registration {
            cleanup: Some(Box::new(move || {
                remove_waiter(&inner, &cleanup_key, id);
            })),
        };
        Box::pin(async move {
            let _registration = registration;
            match signal {
                None => receiver.await.unwrap_or(Err(WaitError::Closed)),
                Some(signal) => {
                    tokio::select! {
                        biased;
                        _ = signal.cancelled() => Err(WaitError::Aborted),
                        result = receiver => result.unwrap_or(Err(WaitError::Closed)),
                    }
                }
            }
        })
    }

    /// Keys with at least one pending waiter.
    pub fn keys(&self) -> Vec<K> {
        match self.inner.state.lock() {
            Ok(state) => state.keys().cloned().collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Resolve every waiter of `key` with `value`.
    pub fn resolve(&self, key: K, value: T) {
        let waiters = match self.inner.state.lock() {
            Ok(mut state) => state.remove(&key).unwrap_or_default(),
            Err(_) => return,
        };
        for waiter in waiters {
            let _ = waiter.sender.send(Ok(value.clone()));
        }
    }

    /// Reject every pending waiter with `error`.
    pub fn reject_all(&self, error: WaitError) {
        let waiters = match self.inner.state.lock() {
            Ok(mut state) => std::mem::take(&mut *state),
            Err(_) => return,
        };
        for (_, entries) in waiters {
            for waiter in entries {
                let _ = waiter.sender.send(Err(error.clone()));
            }
        }
    }
}

/// Removes one waiter upon drop so a cancelled or abandoned future leaks nothing.
struct Registration {
    cleanup: Option<Box<dyn FnOnce() + Send + 'static>>,
}

impl Drop for Registration {
    fn drop(&mut self) {
        if let Some(cleanup) = self.cleanup.take() {
            cleanup();
        }
    }
}

fn remove_waiter<K, T>(inner: &Arc<Inner<K, T>>, key: &K, id: u64)
where
    K: Eq + Hash + Clone,
{
    let Ok(mut state) = inner.state.lock() else {
        return;
    };
    if let Some(entries) = state.get_mut(key) {
        entries.retain(|waiter| waiter.id != id);
        if entries.is_empty() {
            state.remove(key);
        }
    }
}

/// Every item of a paginated scan, in page order.
pub async fn scan_all<T, F, Fut>(mut scan: F) -> SessionResult<Vec<T>>
where
    F: FnMut(Option<Cursor>) -> Fut,
    Fut: Future<Output = SessionResult<Page<T>>>,
{
    let mut items = Vec::new();
    let mut cursor: Option<Cursor> = None;
    loop {
        let page = scan(cursor).await?;
        items.extend(page.items);
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    Ok(items)
}

/// Build the harness-closed error used by pending waits.
pub fn closed_error_session() -> SessionError {
    SessionError::message("Harness is closed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chord::context::{with_cancel, without_abort_signal};
    use crate::chord::json::JsonValue;

    #[tokio::test]
    async fn resolve_settles_all_waiters_of_a_key() {
        let waiters: Waiters<u64, u64> = Waiters::new();
        let context = without_abort_signal(&crate::chord::context::BACKGROUND_CONTEXT);
        let first = waiters.add(1, &context);
        let second = waiters.add(1, &context);
        assert_eq!(waiters.keys(), vec![1]);
        waiters.resolve(1, 42);
        assert_eq!(first.await, Ok(42));
        assert_eq!(second.await, Ok(42));
        assert!(waiters.keys().is_empty());
    }

    #[tokio::test]
    async fn context_cancellation_rejects_only_that_wait() {
        let waiters: Waiters<u64, u64> = Waiters::new();
        let (context, handle) = with_cancel(&crate::chord::context::BACKGROUND_CONTEXT);
        let cancelled = waiters.add(1, &context);
        let kept = waiters.add(1, &crate::chord::context::BACKGROUND_CONTEXT);
        handle.cancel();
        assert_eq!(cancelled.await, Err(WaitError::Aborted));
        waiters.resolve(1, 7);
        assert_eq!(kept.await, Ok(7));
    }

    #[tokio::test]
    async fn reject_all_closes_every_key() {
        let waiters: Waiters<u64, JsonValue> = Waiters::new();
        let context = crate::chord::context::BACKGROUND_CONTEXT.clone();
        let a = waiters.add(1, &context);
        let b = waiters.add(2, &context);
        waiters.reject_all(closed_error());
        assert_eq!(a.await, Err(WaitError::Closed));
        assert_eq!(b.await, Err(WaitError::Closed));
    }
    #[tokio::test]
    async fn reject_all_preserves_session_failure_cause() {
        let waiters: Waiters<u64, JsonValue> = Waiters::new();
        let wait = waiters.add(1, &crate::chord::context::BACKGROUND_CONTEXT);
        waiters.reject_all(WaitError::SessionFailed("storage read failed".to_string()));
        assert_eq!(
            wait.await,
            Err(WaitError::SessionFailed("storage read failed".to_string()))
        );
    }

    #[tokio::test]
    async fn scan_all_follows_cursors() {
        let pages = std::sync::Mutex::new(vec![
            Page {
                items: vec![1, 2],
                next: Some(Cursor {
                    after: 2,
                    order: None,
                }),
            },
            Page {
                items: vec![3],
                next: None,
            },
        ]);
        let items = scan_all(|_cursor| {
            let page = pages.lock().unwrap().remove(0);
            async move { Ok(page) }
        })
        .await
        .unwrap();
        assert_eq!(items, vec![1, 2, 3]);
    }
}
