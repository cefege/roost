//! Document/state service subset (port of `chord/src/services/state.ts` and the
//! `replicatedState` factory in `chord/src/api.ts` @ `abe508e1b`).
//!
//! V17-13 carries the consumed surface: mutable and attached replicated states,
//! publication ordering, source attachments with cursor checks, and operation
//! batches for storage commits. Listeners are synchronous in Rust; upstream
//! supports async listeners with bounded serialized delivery (V17-13 §7.3 form
//! difference).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use thiserror::Error;

use crate::chord::context::Context;
use crate::chord::delta::{Op, apply_immutable, diff_json, is_base};
use crate::chord::json::JsonValue;

/// Delivery kind of one state publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryKind {
    /// Initial hydration of a subscriber.
    Hydrate,
    /// One later revision.
    Update,
}

/// Delivery metadata of one state callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplicatedStateDelivery {
    /// Delivery kind.
    pub kind: DeliveryKind,
    /// Publication sequence.
    pub sequence: u64,
}

/// State listener failure surface.
#[derive(Debug, Clone, Error)]
pub enum StateError {
    /// The state was changed reentrantly from a change callback.
    #[error("Replicated state cannot be changed reentrantly from a change callback")]
    ReentrantChange,
    /// A listener callback failed.
    #[error("Replicated state listeners failed: {0}")]
    Listener(String),
    /// The source contract failed.
    #[error("{0}")]
    Source(String),
}

/// Synchronous state listener (V17-13 §7.3: async listeners stay with V17-14).
pub type StateListener = Arc<dyn Fn(&JsonValue, &Context, &ReplicatedStateDelivery) + Send + Sync>;
/// Source-contract and listener failure handler.
pub type StateErrorHandler = Arc<dyn Fn(StateError) + Send + Sync>;
/// Idempotent source disposal callback.
pub type DisposeCallback = Box<dyn Fn() + Send + Sync>;
/// Single-use frame listener installed by `ReplicatedStateSourceAttachment`.
pub type FrameListener = Box<dyn FnMut(ReplicatedStateSourceFrame) + Send>;
/// Single-use activation callback for an attached source.
pub type ActivateCallback = Box<dyn FnOnce(FrameListener)>;
/// Source listener receiving immutable operation batches.
pub type SourceListener = Arc<dyn Fn(&[Op], u64, &Context) + Send + Sync>;

/// Unsubscribe handle; idempotent.
pub struct Subscription {
    unsubscribe: Box<dyn Fn() + Send + Sync>,
}

impl Subscription {
    /// Stop future callbacks.
    pub fn unsubscribe(&self) {
        (self.unsubscribe)();
    }
}

struct Subscriber {
    id: u64,
    hydrated_sequence: u64,
    listener: StateListener,
}

struct Publication {
    value: JsonValue,
    ops: Vec<Op>,
    sequence: u64,
    context: Context,
}

#[derive(Default)]
struct PublisherInner {
    value: Option<JsonValue>,
    sequence: u64,
    subscribers: Vec<Subscriber>,
    source_listeners: Vec<SourceListener>,
    publications: VecDeque<Publication>,
    delivering: bool,
    next_subscriber: u64,
}

/// Shared publication ordering for mutable and attached states.
#[derive(Clone)]
struct Publisher {
    inner: Arc<Mutex<PublisherInner>>,
}

impl Publisher {
    fn new(value: JsonValue) -> Self {
        Self {
            inner: Arc::new(Mutex::new(PublisherInner {
                value: Some(value),
                ..PublisherInner::default()
            })),
        }
    }

    fn value(&self) -> Option<JsonValue> {
        self.inner.lock().ok().and_then(|inner| inner.value.clone())
    }

    fn subscribe(&self, listener: StateListener) -> Subscription {
        let (id, hydrated, value) = match self.inner.lock() {
            Ok(mut inner) => {
                let id = inner.next_subscriber;
                inner.next_subscriber += 1;
                let hydrated = inner.sequence;
                let value = inner.value.clone();
                inner.subscribers.push(Subscriber {
                    id,
                    hydrated_sequence: hydrated,
                    listener: listener.clone(),
                });
                (id, hydrated, value)
            }
            Err(_) => {
                return Subscription {
                    unsubscribe: Box::new(|| {}),
                };
            }
        };
        // Immediate hydration delivery (upstream `subscribe` pushes and drains).
        if let Some(value) = value {
            let delivery = ReplicatedStateDelivery {
                kind: DeliveryKind::Hydrate,
                sequence: hydrated,
            };
            listener(
                &value,
                &crate::chord::context::BACKGROUND_CONTEXT,
                &delivery,
            );
        }
        let inner = Arc::clone(&self.inner);
        Subscription {
            unsubscribe: Box::new(move || {
                if let Ok(mut inner) = inner.lock() {
                    inner.subscribers.retain(|subscriber| subscriber.id != id);
                }
            }),
        }
    }

    fn subscribe_source(&self, listener: SourceListener) -> Subscription {
        let inner = Arc::clone(&self.inner);
        if let Ok(mut guard) = inner.lock() {
            guard.source_listeners.push(Arc::clone(&listener));
        }
        Subscription {
            unsubscribe: Box::new(move || {
                if let Ok(mut inner) = inner.lock() {
                    inner
                        .source_listeners
                        .retain(|existing| !Arc::ptr_eq(existing, &listener));
                }
            }),
        }
    }

    /// Publish one revision and its operation batch; returns listener failures.
    fn publish(&self, value: JsonValue, ops: Vec<Op>, context: &Context) -> Vec<String> {
        let start_delivery = match self.inner.lock() {
            Ok(mut inner) => {
                inner.value = Some(value.clone());
                inner.sequence += 1;
                let sequence = inner.sequence;
                inner.publications.push_back(Publication {
                    value,
                    ops,
                    sequence,
                    context: context.clone(),
                });
                if inner.delivering {
                    return Vec::new();
                }
                inner.delivering = true;
                true
            }
            Err(_) => return vec!["Replicated state mutex poisoned".to_string()],
        };
        if !start_delivery {
            return Vec::new();
        }
        let errors = Vec::new();
        loop {
            let publication = match self.inner.lock() {
                Ok(mut inner) => {
                    let publication = inner.publications.pop_front();
                    if publication.is_none() {
                        inner.delivering = false;
                    }
                    publication
                }
                Err(_) => None,
            };
            let Some(publication) = publication else {
                break;
            };
            let (source_listeners, subscribers) = match self.inner.lock() {
                Ok(inner) => (
                    inner.source_listeners.clone(),
                    inner
                        .subscribers
                        .iter()
                        .map(|subscriber| {
                            (subscriber.hydrated_sequence, subscriber.listener.clone())
                        })
                        .collect::<Vec<_>>(),
                ),
                Err(_) => (Vec::new(), Vec::new()),
            };
            for listener in source_listeners {
                listener(&publication.ops, publication.sequence, &publication.context);
            }
            let delivery = ReplicatedStateDelivery {
                kind: DeliveryKind::Update,
                sequence: publication.sequence,
            };
            for (hydrated_sequence, listener) in subscribers {
                if publication.sequence <= hydrated_sequence {
                    continue;
                }
                listener(&publication.value, &publication.context, &delivery);
            }
        }
        errors
    }
}

/// Mutable replicated state (`replicatedState(initial)` upstream overload).
#[derive(Clone)]
pub struct MutableReplicatedState {
    publisher: Publisher,
    changing: Arc<AtomicBool>,
}

impl MutableReplicatedState {
    /// Create state taking ownership of `initial`.
    pub fn new(initial: JsonValue) -> Self {
        Self {
            publisher: Publisher::new(initial),
            changing: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Current value.
    pub fn value(&self) -> JsonValue {
        self.publisher.value().unwrap_or(JsonValue::Null)
    }

    /// Atomically publish one synchronous overlay mutation.
    pub fn change(
        &self,
        context: &Context,
        mutate: impl FnOnce(&mut JsonValue),
    ) -> Result<(), StateError> {
        if self.changing.swap(true, Ordering::SeqCst) {
            return Err(StateError::ReentrantChange);
        }
        let previous = self.value();
        let mut next = previous.clone();
        mutate(&mut next);
        self.changing.store(false, Ordering::SeqCst);
        if previous == next {
            return Ok(());
        }
        let ops = diff_json(&previous, &next);
        self.publisher.publish(next, ops, context);
        Ok(())
    }

    /// Atomically take ownership of a replacement value.
    pub fn replace(&self, context: &Context, value: JsonValue) -> Result<(), StateError> {
        let previous = self.value();
        if previous == value {
            return Ok(());
        }
        let ops = diff_json(&previous, &value);
        self.publisher.publish(value, ops, context);
        Ok(())
    }

    /// Subscribe with immediate hydration.
    pub fn subscribe(&self, listener: StateListener) -> Subscription {
        self.publisher.subscribe(listener)
    }

    /// Subscribe to the operation batches of every publication.
    pub fn subscribe_source(&self, listener: SourceListener) -> Subscription {
        self.publisher.subscribe_source(listener)
    }
}

/// Snapshot of an authoritative source at the atomic attachment boundary.
#[derive(Debug, Clone)]
pub struct ReplicatedStateSourceSnapshot {
    /// Immutable snapshot value.
    pub value: JsonValue,
    /// Monotonic source cursor.
    pub cursor: u64,
}

/// One committed source frame.
#[derive(Debug, Clone)]
pub struct ReplicatedStateSourceFrame {
    /// Monotonic source cursor; the first frame after a snapshot must be
    /// `snapshot.cursor + 1`.
    pub cursor: u64,
    /// Exact immutable value produced by this commit.
    pub value: JsonValue,
    /// Exact immutable operation batch that produced `value`.
    pub ops: Vec<Op>,
    /// Frame context.
    pub context: Context,
}

/// One source attachment: fixed snapshot plus a single-use frame listener.
pub struct ReplicatedStateSourceAttachment {
    /// Fixed snapshot captured at the atomic attachment boundary.
    pub snapshot: ReplicatedStateSourceSnapshot,
    /// Install the sole listener and drain every buffered frame in order.
    pub activate: ActivateCallback,
    /// Stop delivery and release source resources; idempotent.
    pub dispose: DisposeCallback,
}

/// An authoritative immutable revision source.
pub trait ReplicatedStateSource: Send + Sync {
    /// Synchronously capture one snapshot and register an attachment.
    fn attach(&self) -> ReplicatedStateSourceAttachment;
}

/// Synchronously hydrated publication-only state backed by one attachment.
pub struct AttachedReplicatedState {
    publisher: Publisher,
    dispose_fn: Arc<Mutex<Option<DisposeCallback>>>,
    disposed: Arc<AtomicBool>,
    #[allow(dead_code)]
    on_error: StateErrorHandler,
}

impl AttachedReplicatedState {
    /// Current value.
    pub fn value(&self) -> JsonValue {
        self.publisher.value().unwrap_or(JsonValue::Null)
    }

    /// Subscribe with immediate hydration.
    pub fn subscribe(&self, listener: StateListener) -> Subscription {
        self.publisher.subscribe(listener)
    }

    /// Subscribe to the operation batches of every publication.
    pub fn subscribe_source(&self, listener: SourceListener) -> Subscription {
        self.publisher.subscribe_source(listener)
    }

    /// Idempotently release the source attachment.
    pub fn dispose(&self) {
        if self.disposed.swap(true, Ordering::SeqCst) {
            return;
        }
        let dispose = self
            .dispose_fn
            .lock()
            .ok()
            .and_then(|mut guard| guard.take());
        if let Some(dispose) = dispose {
            dispose();
        }
    }
}

/// Attach a publication-only replicated state to one authoritative source
/// stream (upstream `attachReplicatedStateSource`).
pub fn attach_replicated_state_source(
    source: &dyn ReplicatedStateSource,
    options: ReplicatedStateSourceOptions,
) -> AttachedReplicatedState {
    let on_error: StateErrorHandler = options
        .on_error
        .unwrap_or_else(|| Arc::new(|error| eprintln!("replicated state error: {error}")));
    let attachment = source.attach();
    let snapshot = attachment.snapshot.clone();
    let publisher = Publisher::new(snapshot.value);
    let cursor = Arc::new(Mutex::new(snapshot.cursor));
    let disposed = Arc::new(AtomicBool::new(false));
    let dispose_fn = Arc::new(Mutex::new(Some(attachment.dispose)));
    let state = AttachedReplicatedState {
        publisher: publisher.clone(),
        dispose_fn: Arc::clone(&dispose_fn),
        disposed: Arc::clone(&disposed),
        on_error: Arc::clone(&on_error),
    };
    let publisher_for_frames = publisher.clone();
    let cursor_for_frames = Arc::clone(&cursor);
    let disposed_for_frames = Arc::clone(&disposed);
    let on_error_for_frames = Arc::clone(&on_error);
    let activate = attachment.activate;
    activate(Box::new(move |frame| {
        if disposed_for_frames.load(Ordering::SeqCst) {
            return;
        }
        let expected = cursor_for_frames
            .lock()
            .map(|cursor| *cursor + 1)
            .unwrap_or(u64::MAX);
        if frame.cursor != expected {
            disposed_for_frames.store(true, Ordering::SeqCst);
            on_error_for_frames(StateError::Source(format!(
                "Replicated state source cursor has a gap: expected {expected}, received {}",
                frame.cursor
            )));
            return;
        }
        if let Ok(mut cursor) = cursor_for_frames.lock() {
            *cursor = frame.cursor;
        }
        let errors = publisher_for_frames.publish(frame.value, frame.ops, &frame.context);
        if !errors.is_empty() {
            on_error_for_frames(StateError::Listener(errors.join("; ")));
        }
    }));
    state
}

/// Options for [`attach_replicated_state_source`].
#[derive(Default)]
pub struct ReplicatedStateSourceOptions {
    /// Receives source-contract and publication-listener failures.
    pub on_error: Option<StateErrorHandler>,
}

/// Cold read-only replica used until a complete snapshot arrives
/// (upstream `ReplicatedStateReplica`; remote-service consumption stays
/// [DEFER] with protocol, FR-B R5).
pub struct ReplicatedStateReplica {
    value: Option<JsonValue>,
    sequence: Option<u64>,
    #[allow(dead_code)]
    on_error: StateErrorHandler,
}

impl ReplicatedStateReplica {
    /// Create an empty replica.
    pub fn new(on_error: Arc<dyn Fn(StateError) + Send + Sync>) -> Self {
        Self {
            value: None,
            sequence: None,
            on_error,
        }
    }

    /// Hydrate from a base operation batch; sequence must start the stream.
    pub fn hydrate(&mut self, sequence: u64, ops: &[Op]) -> Result<(), StateError> {
        if !is_base(ops) {
            return Err(StateError::Source(
                "Replicated state snapshot is not a base operation batch".to_string(),
            ));
        }
        let value =
            apply_immutable(None, ops).map_err(|error| StateError::Source(error.to_string()))?;
        self.value = Some(value);
        self.sequence = Some(sequence);
        Ok(())
    }

    /// Apply one later operation batch; rejects gaps.
    pub fn update(&mut self, sequence: u64, ops: &[Op]) -> Result<(), StateError> {
        let Some(current) = &self.value else {
            return Err(StateError::Source(
                "Replicated state received an update before hydration".to_string(),
            ));
        };
        let Some(current_sequence) = self.sequence else {
            return Err(StateError::Source(
                "Replicated state received an update before hydration".to_string(),
            ));
        };
        if sequence != current_sequence + 1 {
            self.clear();
            return Err(StateError::Source(
                "Replicated state update sequence has a gap".to_string(),
            ));
        }
        let value = apply_immutable(Some(current), ops)
            .map_err(|error| StateError::Source(error.to_string()))?;
        self.value = Some(value);
        self.sequence = Some(sequence);
        Ok(())
    }

    /// Drop hydrated state after a gap or validation failure.
    pub fn clear(&mut self) {
        self.value = None;
        self.sequence = None;
    }

    /// Hydrated value, when present.
    pub fn value(&self) -> Option<&JsonValue> {
        self.value.as_ref()
    }
}
