//! Session-to-Chord observation bridge (port of
//! `packages/durable/src/session/observation.ts` @ `abe508e1b`).
//!
//! Upstream attaches one `CommittedStateSource` (for a read-only replicated
//! state) or one `CommittedWatch` (for a serialized async frame watch) to a
//! committed document incarnation. This module carries both.
//!
//! Shape differences (V17-14 §7.3): the bounded pending queue collapses to one
//! root replacement exactly like upstream; async listener delivery is performed
//! by a single spawned drain task per watch instead of microtasks, so frame
//! ordering is preserved through the queue but microtask turn boundaries are not
//! observable; `queueMicrotask` maps to `tokio::sync::Notify`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::chord::context::{Context, without_abort_signal};
use crate::chord::delta::Op;
use crate::chord::json::{JsonObject, JsonValue};
use crate::chord::state::{
    AttachedReplicatedState, FrameListener, ReplicatedStateSource, ReplicatedStateSourceAttachment,
    ReplicatedStateSourceFrame, ReplicatedStateSourceOptions, ReplicatedStateSourceSnapshot,
    attach_replicated_state_source,
};

/// Maximum exact committed frames retained behind one unavailable watch listener.
pub const MAX_PENDING_WATCH_FRAMES: usize = 100;

/// Canonical terminal update for a retired document incarnation (upstream
/// `RETIREMENT_OPERATIONS`).
pub const RETIREMENT_OPERATIONS: &[Op] = &[Op::Replace(JsonValue::Null)];

/// Observed document value: the committed root object, or `null` once retired.
pub type ObservedDocumentValue = Option<JsonObject>;

/// A boxed watch-listener future; `Err` settles the watch with `listener_error`.
pub type WatchFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'static>>;

/// One serialized asynchronous watch listener (upstream `WatchHandle.start`).
pub type WatchListener =
    Arc<dyn Fn(ObservedDocumentValue, Vec<Op>, Context) -> WatchFuture + Send + Sync>;

/// Reason a watch terminated (upstream `WatchEnd`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEnd {
    /// Stopped by its owner.
    Stopped,
    /// Cancelled through the caller's abort signal.
    Cancelled,
    /// The session closed.
    SessionClosed,
    /// The observed incarnation retired.
    Retired,
    /// The listener threw; the affected watch settles with this message.
    ListenerError(String),
}

fn to_observed(value: &JsonValue) -> ObservedDocumentValue {
    match value {
        JsonValue::Null => None,
        JsonValue::Object(object) => Some(object.clone()),
        // A document root is always an object; any other shape is treated as absent.
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// CommittedWatch
// ---------------------------------------------------------------------------

struct WatchFrame {
    value: JsonValue,
    ops: Vec<Op>,
    context: Context,
}

#[derive(Default)]
struct WatchState {
    value: JsonValue,
    pending: VecDeque<WatchFrame>,
    listener: Option<WatchListener>,
    started: bool,
    draining: bool,
    end: Option<WatchEnd>,
    retired: bool,
    detach: Option<Box<dyn Fn() + Send + Sync>>,
}

/// Serialized exact-frame watch bound to one document incarnation (upstream
/// `CommittedWatch`).
#[derive(Clone)]
pub struct CommittedWatch {
    inner: Arc<Mutex<WatchState>>,
    notify: Arc<tokio::sync::Notify>,
}

impl CommittedWatch {
    /// Create a watch holding the acquisition revision value.
    pub fn new(value: JsonValue) -> Self {
        Self {
            inner: Arc::new(Mutex::new(WatchState {
                value,
                ..WatchState::default()
            })),
            notify: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// Install the unsubscribe callback invoked when the watch terminates.
    pub(crate) fn set_detach(&self, detach: Box<dyn Fn() + Send + Sync>) {
        if let Ok(mut state) = self.inner.lock() {
            state.detach = Some(detach);
        }
    }

    /// Current observed value; `None` after retirement.
    pub fn value(&self) -> ObservedDocumentValue {
        let value = self
            .inner
            .lock()
            .map(|state| state.value.clone())
            .unwrap_or(JsonValue::Null);
        to_observed(&value)
    }

    /// Whether the watch has terminated.
    pub fn is_closed(&self) -> bool {
        self.inner
            .lock()
            .map(|state| state.end.is_some())
            .unwrap_or(true)
    }

    /// Start serialized delivery. Fails when already started or already stopped
    /// (upstream throws).
    pub fn start(&self, listener: WatchListener) -> Result<(), crate::session::SessionError> {
        let has_pending = {
            let mut state = self
                .inner
                .lock()
                .map_err(|_| crate::session::SessionError::message("watch lock poisoned"))?;
            if state.started {
                return Err(crate::session::SessionError::message(
                    "Watch is already started",
                ));
            }
            if state.end.is_some() {
                return Err(crate::session::SessionError::message("Watch is stopped"));
            }
            state.started = true;
            state.listener = Some(listener);
            !state.pending.is_empty()
        };
        if has_pending {
            self.spawn_drain();
        }
        Ok(())
    }

    /// Stop delivery and await the terminal reason. Idempotent.
    pub async fn stop(&self) -> WatchEnd {
        self.terminate(WatchEnd::Stopped);
        self.closed().await
    }

    /// Cancel through the caller's abort signal.
    pub fn cancel(&self) {
        self.terminate(WatchEnd::Cancelled);
    }

    /// Terminate because the session closed.
    pub fn close_session(&self) {
        self.terminate(WatchEnd::SessionClosed);
    }

    /// Await the terminal reason.
    pub async fn closed(&self) -> WatchEnd {
        loop {
            let notified = self.notify.notified();
            {
                if let Ok(state) = self.inner.lock()
                    && let Some(end) = state.end.clone()
                {
                    return end;
                }
            }
            notified.await;
        }
    }

    /// Deliver one committed frame; buffers or collapses on overflow.
    pub(crate) fn advance(&self, value: JsonValue, ops: Vec<Op>, context: Context) {
        let notify_started = {
            let Ok(mut state) = self.inner.lock() else {
                return;
            };
            if state.end.is_some() || state.retired {
                return;
            }
            if value.is_null() {
                state.retired = true;
            }
            if state.pending.len() >= MAX_PENDING_WATCH_FRAMES {
                // Overflow collapses every buffered frame into one root
                // replacement of the newest value (or the retirement marker).
                state.pending.clear();
                state.pending.push_back(WatchFrame {
                    value: value.clone(),
                    ops: vec![Op::Replace(value.clone())],
                    context,
                });
            } else {
                state.pending.push_back(WatchFrame {
                    value,
                    ops,
                    context,
                });
            }
            state.started
        };
        if notify_started {
            self.spawn_drain();
        }
    }
    fn spawn_drain(&self) {
        let should_spawn = {
            let Ok(mut state) = self.inner.lock() else {
                return;
            };
            if state.end.is_some() {
                return;
            }
            if state.draining {
                false
            } else {
                state.draining = true;
                true
            }
        };
        self.notify.notify_one();
        if should_spawn {
            let inner = Arc::clone(&self.inner);
            let notify = Arc::clone(&self.notify);
            tokio::spawn(async move {
                drain(inner, notify).await;
            });
        }
    }

    fn terminate(&self, end: WatchEnd) {
        let detach = {
            let Ok(mut state) = self.inner.lock() else {
                return;
            };
            if state.end.is_some() {
                return;
            }
            state.end = Some(end);
            state.pending.clear();
            state.detach.take()
        };
        if let Some(detach) = detach {
            detach();
        }
        self.notify.notify_waiters();
    }
}

async fn drain(inner: Arc<Mutex<WatchState>>, notify: Arc<tokio::sync::Notify>) {
    loop {
        let notified = notify.notified();
        // Drain every buffered frame.
        loop {
            let frame = {
                let Ok(mut state) = inner.lock() else {
                    return;
                };
                if state.end.is_some() {
                    state.draining = false;
                    return;
                }
                state.pending.pop_front()
            };
            let Some(frame) = frame else {
                break;
            };
            let listener = {
                let Ok(mut state) = inner.lock() else {
                    return;
                };
                state.value = frame.value.clone();
                state.listener.clone()
            };
            let Some(listener) = listener else {
                continue;
            };
            let observed = to_observed(&frame.value);
            let result = listener(
                observed,
                frame.ops.clone(),
                without_abort_signal(&frame.context),
            )
            .await;
            let mut state = match inner.lock() {
                Ok(state) => state,
                Err(_) => return,
            };
            if state.end.is_some() {
                state.draining = false;
                drop(state);
                return;
            }
            if let Err(message) = result {
                state.end = Some(WatchEnd::ListenerError(message));
                state.pending.clear();
            } else if frame.value.is_null() {
                state.end = Some(WatchEnd::Retired);
                state.pending.clear();
            } else {
                drop(state);
                continue;
            }
            let detach = state.detach.take();
            state.draining = false;
            drop(state);
            if let Some(detach) = detach {
                detach();
            }
            notify.notify_waiters();
            return;
        }
        // Wait for more frames or termination.
        {
            let Ok(state) = inner.lock() else {
                return;
            };
            if state.end.is_some() {
                continue;
            }
        }
        notified.await;
    }
}

// ---------------------------------------------------------------------------
// CommittedStateSource
// ---------------------------------------------------------------------------

struct AttachmentSlot {
    id: u64,
    listener: Option<FrameListener>,
}

struct SourceInner {
    attachments: Mutex<Vec<AttachmentSlot>>,
    value: Mutex<JsonValue>,
    cursor: AtomicU64,
    retired: AtomicBool,
    closed: AtomicBool,
    release: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    next_id: AtomicU64,
}

/// Session-to-Chord bridge owned one-to-one by one attached document state
/// (upstream `CommittedStateSource`). A `null` value retires it.
#[derive(Clone)]
pub struct CommittedStateSource {
    inner: Arc<SourceInner>,
}

impl CommittedStateSource {
    /// Create a source over one committed value.
    pub fn new(value: JsonValue) -> Self {
        Self {
            inner: Arc::new(SourceInner {
                attachments: Mutex::new(Vec::new()),
                value: Mutex::new(value),
                cursor: AtomicU64::new(0),
                retired: AtomicBool::new(false),
                closed: AtomicBool::new(false),
                release: Mutex::new(None),
                next_id: AtomicU64::new(0),
            }),
        }
    }

    /// Install the callback invoked when every attachment is disposed.
    pub(crate) fn set_release(&self, release: Box<dyn Fn() + Send + Sync>) {
        if let Ok(mut slot) = self.inner.release.lock() {
            *slot = Some(release);
        }
    }

    /// Forward one committed frame to every attachment (upstream `advance`).
    pub(crate) fn advance(&self, value: JsonValue, ops: Vec<Op>, context: Context) {
        if self.inner.closed.load(Ordering::SeqCst) || self.inner.retired.load(Ordering::SeqCst) {
            return;
        }
        if let Ok(mut current) = self.inner.value.lock() {
            *current = value.clone();
        }
        let cursor = self.inner.cursor.fetch_add(1, Ordering::SeqCst) + 1;
        if value.is_null() {
            self.inner.retired.store(true, Ordering::SeqCst);
        }
        let frame = ReplicatedStateSourceFrame {
            cursor,
            value,
            ops,
            context,
        };
        if let Ok(mut attachments) = self.inner.attachments.lock() {
            for slot in attachments.iter_mut() {
                if let Some(listener) = slot.listener.as_mut() {
                    listener(frame.clone());
                }
            }
        }
    }

    /// Release every attachment because the session closed (upstream
    /// `closeSession`).
    pub(crate) fn close_session(&self) {
        if self.inner.closed.load(Ordering::SeqCst) {
            return;
        }
        if let Ok(mut attachments) = self.inner.attachments.lock() {
            attachments.clear();
        }
        self.finish_disposal();
    }

    fn remove_attachment(&self, id: u64) {
        let empty = match self.inner.attachments.lock() {
            Ok(mut attachments) => {
                attachments.retain(|slot| slot.id != id);
                attachments.is_empty()
            }
            Err(_) => false,
        };
        if empty {
            self.finish_disposal();
        }
    }

    fn finish_disposal(&self) {
        if self.inner.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Ok(mut value) = self.inner.value.lock() {
            *value = JsonValue::Null;
        }
        let release = self
            .inner
            .release
            .lock()
            .ok()
            .and_then(|mut slot| slot.take());
        if let Some(release) = release {
            release();
        }
    }
}

impl ReplicatedStateSource for CommittedStateSource {
    fn attach(&self) -> ReplicatedStateSourceAttachment {
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let snapshot = ReplicatedStateSourceSnapshot {
            value: self
                .inner
                .value
                .lock()
                .map(|value| value.clone())
                .unwrap_or(JsonValue::Null),
            cursor: self.inner.cursor.load(Ordering::SeqCst),
        };
        if let Ok(mut attachments) = self.inner.attachments.lock() {
            attachments.push(AttachmentSlot { id, listener: None });
        }
        let this = self.clone();
        let activate = Box::new(move |listener: FrameListener| {
            if let Ok(mut attachments) = this.inner.attachments.lock()
                && let Some(slot) = attachments.iter_mut().find(|slot| slot.id == id)
            {
                slot.listener = Some(listener);
            }
        });
        let this = self.clone();
        let dispose = Box::new(move || {
            this.remove_attachment(id);
        });
        ReplicatedStateSourceAttachment {
            snapshot,
            activate,
            dispose,
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers used by the session
// ---------------------------------------------------------------------------

/// Attach a read-only replicated state to one committed document value
/// (upstream `replicatedState(source)`).
pub(crate) fn attach_state(source: &CommittedStateSource) -> AttachedReplicatedState {
    attach_replicated_state_source(source, ReplicatedStateSourceOptions::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    fn listener(log: Arc<StdMutex<Vec<ObservedDocumentValue>>>) -> WatchListener {
        Arc::new(move |value, _ops, _context| {
            let log = Arc::clone(&log);
            Box::pin(async move {
                log.lock().expect("log").push(value);
                Ok(())
            })
        })
    }

    #[tokio::test]
    async fn overflow_collapses_to_one_root_replacement() {
        let watch = CommittedWatch::new(serde_json::json!({ "value": 0 }));
        for value in 1..=101 {
            watch.advance(
                serde_json::json!({ "value": value }),
                vec![Op::Set {
                    path: vec![crate::chord::delta::PathSegment::Key("value".into())],
                    value: serde_json::json!(value),
                }],
                crate::chord::context::BACKGROUND_CONTEXT.clone(),
            );
        }
        let log = Arc::new(StdMutex::new(Vec::new()));
        watch.start(listener(Arc::clone(&log))).expect("start");
        // Allow the drain task to run.
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let delivered = log.lock().expect("log").clone();
        assert_eq!(delivered.len(), 1);
        assert_eq!(
            delivered[0],
            Some(
                serde_json::json!({ "value": 101 })
                    .as_object()
                    .expect("object")
                    .clone()
            )
        );
        let _ = watch.stop().await;
    }
}
