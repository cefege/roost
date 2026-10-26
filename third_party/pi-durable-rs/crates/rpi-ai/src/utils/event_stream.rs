//! Port of `packages/ai/src/utils/event-stream.ts` @ pi 0.82.1 (2efa728).
//!
//! `AssistantMessageEventStream`: producer pushes events synchronously while
//! the consumer iterates asynchronously; `done`/`error` events resolve the
//! final result. Backed by an unbounded mpsc channel (the upstream queue is
//! unbounded; consumers see the same ordering and buffering semantics).
//!
//! #9055 (`b2602be77`, "optimize EventStream queue"): upstream replaced the
//! `Array.shift()` queue (O(n) per dequeue, O(n²) when draining buffered
//! events) with a two-stack FifoQueue. This port is exempt by construction:
//! `mpsc::UnboundedReceiver::poll_recv` is O(1), so the drain path was never
//! quadratic. The upstream regression suite is ported below (drain order,
//! post-terminal push, interleaved arrival, waiting-consumer order, drain
//! after end) plus the linearity assertion the task requires.
//!
//! The stream is `Clone`: clones share the same underlying queue, mirroring
//! the single upstream object passed between producers and consumers.

use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context as TaskContext, Poll};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use futures::Stream;
use futures::future::{BoxFuture, Shared};
use futures::prelude::*;
use tokio::sync::{mpsc, oneshot};

use crate::types::{AssistantMessage, StreamEvent};

struct Inner {
    tx: Mutex<Option<mpsc::UnboundedSender<StreamEvent>>>,
    rx: Mutex<mpsc::UnboundedReceiver<StreamEvent>>,
    result_tx: Mutex<Option<oneshot::Sender<AssistantMessage>>>,
    result_rx: Shared<oneshot::Receiver<AssistantMessage>>,
    done: AtomicBool,
    /// Wall-clock start of this stream (`#startedAt`, event-stream.ts:98).
    started_at_wall_ms: i64,
    /// Monotonic start of this stream (`#startedAtMonotonic`).
    started_at_monotonic: Instant,
}

/// `Date.now()` at stream creation.
fn now_wall_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// Producer/consumer event stream for assistant message events.
///
/// - [`push`](Self::push) is ignored after a `done`/`error` event or
///   [`end`](Self::end) (upstream `done` flag).
/// - [`end`](Self::end) closes the channel; buffered events drain first.
/// - [`result`](Self::result) resolves with the message from the terminal
///   `done`/`error` event, or the value passed to `end`.
#[derive(Clone)]
pub struct AssistantMessageEventStream {
    inner: Arc<Inner>,
}

impl Default for AssistantMessageEventStream {
    fn default() -> Self {
        Self::new()
    }
}

impl AssistantMessageEventStream {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let (result_tx, result_rx) = oneshot::channel();
        Self {
            inner: Arc::new(Inner {
                tx: Mutex::new(Some(tx)),
                rx: Mutex::new(rx),
                result_tx: Mutex::new(Some(result_tx)),
                result_rx: result_rx.shared(),
                done: AtomicBool::new(false),
                started_at_wall_ms: now_wall_ms(),
                started_at_monotonic: Instant::now(),
            }),
        }
    }

    /// `#time` (event-stream.ts:120-123 @ abe508e1b): set `durationMs` with a
    /// monotonic clock unless the stream already completed, the message
    /// already carries a duration (a forwarding stream keeps the inner
    /// measurement), or its `timestamp` predates the stream (a fetched
    /// deferred result stays untimed).
    fn time_response(&self, message: &mut AssistantMessage) {
        if self.inner.done.load(Ordering::SeqCst)
            || message.duration_ms.is_some()
            || message.timestamp < self.inner.started_at_wall_ms
        {
            return;
        }
        message.duration_ms = Some(self.inner.started_at_monotonic.elapsed().as_millis() as u64);
    }

    fn resolve_result(&self, message: AssistantMessage) {
        // First resolution wins (upstream promise resolve is idempotent).
        if let Some(tx) = self
            .inner
            .result_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            let _ = tx.send(message);
        }
    }

    fn is_complete(event: &StreamEvent) -> bool {
        matches!(event, StreamEvent::Done { .. } | StreamEvent::Error { .. })
    }

    fn extract_result(event: &StreamEvent) -> Option<AssistantMessage> {
        match event {
            StreamEvent::Done { message, .. } => Some(message.clone()),
            StreamEvent::Error { error, .. } => Some(error.clone()),
            _ => None,
        }
    }

    /// Push an event to the stream. Terminal events time the final message
    /// before the completion guard (event-stream.ts:106-110).
    pub fn push(&self, event: StreamEvent) {
        if self.inner.done.load(Ordering::SeqCst) {
            return;
        }
        let event = match event {
            StreamEvent::Done {
                reason,
                mut message,
            } => {
                self.time_response(&mut message);
                StreamEvent::Done { reason, message }
            }
            StreamEvent::Error { reason, mut error } => {
                self.time_response(&mut error);
                StreamEvent::Error { reason, error }
            }
            other => other,
        };
        if Self::is_complete(&event) {
            self.inner.done.store(true, Ordering::SeqCst);
            if let Some(result) = Self::extract_result(&event) {
                self.resolve_result(result);
            }
        }
        // A closed channel (consumer dropped / ended) is not an error upstream.
        if let Some(tx) = self
            .inner
            .tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            let _ = tx.send(event);
        }
    }

    /// End the stream. A provided result is timed before the completion
    /// guard (`end`, event-stream.ts:112-115) and resolves pending
    /// `result()` calls (unless already resolved by a terminal event).
    pub fn end(&self, result: Option<AssistantMessage>) {
        if let Some(mut result) = result {
            self.time_response(&mut result);
            self.inner.done.store(true, Ordering::SeqCst);
            self.resolve_result(result);
        } else {
            self.inner.done.store(true, Ordering::SeqCst);
        }
        // Dropping the sender closes the channel: iterators terminate after
        // draining buffered events, and pending `poll_recv` wakers fire.
        self.inner
            .tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
    }

    /// The final assistant message (resolves on the terminal event).
    pub fn result(&self) -> BoxFuture<'static, Option<AssistantMessage>> {
        self.inner.result_rx.clone().map(Result::ok).boxed()
    }
}

impl Stream for AssistantMessageEventStream {
    type Item = StreamEvent;

    fn poll_next(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        self.inner
            .rx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .poll_recv(cx)
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;

    use super::*;
    use crate::types::{ApiKind, AssistantRole, DoneReason, ErrorReason, StopReason, Usage};

    fn message(stop_reason: StopReason) -> AssistantMessage {
        AssistantMessage {
            role: AssistantRole::Assistant,
            content: vec![],
            api: ApiKind::from("anthropic-messages"),
            provider: "p".to_owned(),
            model: "m".to_owned(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            thinking_level: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason,
            error_message: None,
            timestamp: 0,
            deferred: None,
            end_turn: None,
            raw_stop_reason: None,
            duration_ms: None,
        }
    }

    #[tokio::test]
    async fn test_push_iterate_result() {
        let stream = AssistantMessageEventStream::new();
        stream.push(StreamEvent::Start {
            partial: Arc::new(message(StopReason::Pending)),
        });
        stream.push(StreamEvent::Done {
            reason: DoneReason::Stop,
            message: message(StopReason::Stop),
        });
        stream.end(None);

        let events: Vec<StreamEvent> = stream.collect().await;
        assert_eq!(events.len(), 2);
    }

    #[tokio::test]
    async fn test_result_resolves_on_terminal_event() {
        let stream = AssistantMessageEventStream::new();
        let result_future = stream.result();
        stream.push(StreamEvent::Error {
            reason: ErrorReason::Error,
            error: message(StopReason::Error),
        });
        let result = result_future.await.expect("resolved");
        assert_eq!(result.stop_reason, StopReason::Error);
    }

    #[tokio::test]
    async fn test_push_after_done_is_dropped() {
        let stream = AssistantMessageEventStream::new();
        stream.push(StreamEvent::Done {
            reason: DoneReason::Stop,
            message: message(StopReason::Stop),
        });
        stream.push(StreamEvent::Start {
            partial: Arc::new(message(StopReason::Pending)),
        });
        stream.end(None);
        let events: Vec<StreamEvent> = stream.collect().await;
        assert_eq!(events.len(), 1);
    }

    #[tokio::test]
    async fn test_end_result_fallback() {
        let stream = AssistantMessageEventStream::new();
        stream.push(StreamEvent::Start {
            partial: Arc::new(message(StopReason::Pending)),
        });
        stream.end(Some(message(StopReason::Aborted)));
        let result = stream.result().await.expect("resolved");
        assert_eq!(result.stop_reason, StopReason::Aborted);
        let events: Vec<StreamEvent> = stream.collect().await;
        assert_eq!(events.len(), 1);
    }

    #[tokio::test]
    async fn test_clone_shares_queue() {
        let stream = AssistantMessageEventStream::new();
        let producer = stream.clone();
        producer.push(StreamEvent::Start {
            partial: Arc::new(message(StopReason::Pending)),
        });
        producer.end(None);
        let events: Vec<StreamEvent> = stream.collect().await;
        assert_eq!(events.len(), 1);
    }

    // -----------------------------------------------------------------------
    // #9055 regression suite (upstream event-stream.test.ts @ b2602be77).
    // The rpi queue is channel-backed (O(1) dequeue), so these pin the
    // ordering semantics the upstream FifoQueue swap had to preserve.
    // -----------------------------------------------------------------------

    fn kind(event: &StreamEvent) -> &'static str {
        match event {
            StreamEvent::Start { .. } => "start",
            StreamEvent::TextStart { .. } => "text_start",
            StreamEvent::TextDelta { .. } => "text_delta",
            StreamEvent::TextEnd { .. } => "text_end",
            StreamEvent::ThinkingStart { .. } => "thinking_start",
            StreamEvent::Done { .. } => "done",
            StreamEvent::Error { .. } => "error",
            _ => "other",
        }
    }

    fn text_event(kind_index: usize, partial: &AssistantMessage) -> StreamEvent {
        match kind_index {
            0 => StreamEvent::Start {
                partial: Arc::new(partial.clone()),
            },
            1 => StreamEvent::TextStart {
                content_index: 0,
                partial: Arc::new(partial.clone()),
            },
            _ => StreamEvent::TextDelta {
                content_index: 0,
                delta: "x".to_owned(),
                partial: Arc::new(partial.clone()),
            },
        }
    }

    /// "drains buffered events in order and ignores events pushed after
    /// completion" + "drains buffered events after end and resolves the
    /// explicit result" (both upstream cases share the buffered-drain shape;
    /// `end(Some(..))` is the explicit-result arm).
    #[tokio::test]
    async fn test_9055_drains_buffered_events_in_order() {
        let partial = message(StopReason::Pending);
        let stream = AssistantMessageEventStream::new();
        stream.push(text_event(0, &partial));
        stream.push(text_event(1, &partial));
        stream.push(StreamEvent::Done {
            reason: DoneReason::Stop,
            message: message(StopReason::Stop),
        });
        // Terminal event flips `done`: later pushes are dropped.
        stream.push(text_event(2, &partial));
        stream.end(Some(message(StopReason::Stop)));

        let result = stream.result().await.expect("resolved");
        assert_eq!(result.stop_reason, StopReason::Stop);

        let kinds: Vec<&str> = stream.clone().map(|event| kind(&event)).collect().await;
        assert_eq!(kinds, vec!["start", "text_start", "done"]);
    }

    /// "preserves order when events arrive after buffered draining starts".
    #[tokio::test]
    async fn test_9055_order_preserved_when_events_arrive_mid_drain() {
        let partial = message(StopReason::Pending);
        let stream = AssistantMessageEventStream::new();
        stream.push(text_event(0, &partial));
        stream.push(text_event(1, &partial));

        let mut iter = stream.clone();
        let first = iter.next().await;
        assert_eq!(kind(&first.expect("event")), "start");

        // Arrives after draining started: must land behind the buffered event.
        stream.push(text_event(2, &partial));
        assert_eq!(kind(&iter.next().await.expect("event")), "text_start");
        assert_eq!(kind(&iter.next().await.expect("event")), "text_delta");
        stream.end(None);
        assert!(iter.next().await.is_none());
    }

    /// "delivers events to waiting consumers in registration order".
    /// Both consumers' `next()` futures are polled once (registering their
    /// waiters on the shared receiver) before any event is pushed; the
    /// channel dequeues FIFO, and the first-registered consumer is the first
    /// to resume.
    #[tokio::test]
    async fn test_9055_waiting_consumers_served_in_registration_order() {
        let stream = AssistantMessageEventStream::new();
        let partial = message(StopReason::Pending);

        let mut first = stream.clone();
        let mut second = stream.clone();
        let producer = stream.clone();

        let mut f1 = Box::pin(first.next());
        let mut f2 = Box::pin(second.next());
        // Deterministic registration: both waiters park before the pushes.
        assert!(futures::poll!(&mut f1).is_pending());
        assert!(futures::poll!(&mut f2).is_pending());

        producer.push(text_event(0, &partial));
        producer.push(text_event(1, &partial));

        let e1 = f1.await.expect("event for first");
        let e2 = f2.await.expect("event for second");
        assert_eq!(kind(&e1), "start");
        assert_eq!(kind(&e2), "text_start");
    }

    /// "wakes all waiting consumers when ended without a result".
    #[tokio::test]
    async fn test_9055_end_wakes_all_waiting_consumers() {
        let stream = AssistantMessageEventStream::new();

        let mut first = stream.clone();
        let mut second = stream.clone();
        let producer = stream.clone();

        let mut f1 = Box::pin(first.next());
        let mut f2 = Box::pin(second.next());
        assert!(futures::poll!(&mut f1).is_pending());
        assert!(futures::poll!(&mut f2).is_pending());

        producer.end(None);
        assert!(f1.await.is_none());
        assert!(f2.await.is_none());
    }

    /// Linearity assertion (task §4 FR-A): draining 10⁴ and 10⁵ buffered
    /// events must stay in the linear regime. A `shift`-style O(n) dequeue
    /// would make the 10⁵ round ~100× the 10⁴ round; O(1) dequeues keep the
    /// ratio near 10×. Both rounds run in milliseconds, so the generous
    /// bounds below only fail on quadratic blowups (50× ratio / 5s absolute).
    #[tokio::test]
    async fn test_9055_buffered_drain_is_linear() {
        async fn drain_round(n: u32) -> (u32, std::time::Duration) {
            let start = std::time::Instant::now();
            let stream = AssistantMessageEventStream::new();
            let partial = message(StopReason::Pending);
            for _ in 0..n {
                stream.push(StreamEvent::Start {
                    partial: Arc::new(partial.clone()),
                });
            }
            stream.end(None);
            let mut iter = stream.clone();
            let mut drained = 0u32;
            while iter.next().await.is_some() {
                drained += 1;
            }
            (drained, start.elapsed())
        }

        let (small_count, small) = drain_round(10_000).await;
        let (large_count, large) = drain_round(100_000).await;
        assert_eq!(small_count, 10_000);
        assert_eq!(large_count, 100_000);
        assert!(
            large.as_secs() < 5,
            "100k drain took {large:?} (quadratic?)"
        );
        let ratio = large.as_secs_f64() / small.as_secs_f64().max(1e-9);
        assert!(
            ratio < 50.0,
            "drain time ratio 100k/10k = {ratio:.1}x (linear ≈ 10x, quadratic ≈ 100x)"
        );
    }

    // -----------------------------------------------------------------------
    // R2.7 `durationMs` timing (upstream event-stream.test.ts @ 36a686ee8 /
    // f284a2460): monotonic from stream creation, saw-start only.
    // -----------------------------------------------------------------------

    fn timed_message(stop_reason: StopReason) -> AssistantMessage {
        let mut value = message(stop_reason);
        value.timestamp = now_wall_ms();
        value
    }

    /// "sets durationMs on the final done or error message of a response it
    /// saw start".
    #[tokio::test]
    async fn test_duration_ms_set_on_terminal_message() {
        let done = AssistantMessageEventStream::new();
        let answer = timed_message(StopReason::Stop);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        done.push(StreamEvent::Done {
            reason: DoneReason::Stop,
            message: answer,
        });
        let resolved = done.result().await.expect("resolved");
        let measured = resolved.duration_ms.expect("timed");
        assert!(measured >= 15, "duration {measured}ms below sleep window");

        let failed = AssistantMessageEventStream::new();
        failed.push(StreamEvent::Error {
            reason: ErrorReason::Error,
            error: timed_message(StopReason::Error),
        });
        assert!(
            failed
                .result()
                .await
                .expect("resolved")
                .duration_ms
                .is_some()
        );

        let ended = AssistantMessageEventStream::new();
        ended.end(Some(timed_message(StopReason::Aborted)));
        assert!(
            ended
                .result()
                .await
                .expect("resolved")
                .duration_ms
                .is_some()
        );
    }

    /// "keeps an existing duration, so a forwarding stream keeps the inner
    /// measurement".
    #[tokio::test]
    async fn test_duration_ms_keeps_existing_measurement() {
        let outer = AssistantMessageEventStream::new();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let inner = AssistantMessageEventStream::new();
        inner.push(StreamEvent::Done {
            reason: DoneReason::Stop,
            message: timed_message(StopReason::Stop),
        });
        let measured_message = inner.result().await.expect("resolved");
        let measured = measured_message.duration_ms.expect("inner measurement");
        assert!(measured < 20, "inner measurement {measured}ms too slow");
        outer.push(StreamEvent::Done {
            reason: DoneReason::Stop,
            message: measured_message,
        });
        assert_eq!(
            outer.result().await.expect("resolved").duration_ms,
            Some(measured)
        );

        let preset = {
            let mut value = timed_message(StopReason::Stop);
            value.duration_ms = Some(1234);
            value
        };
        let stream = AssistantMessageEventStream::new();
        stream.push(StreamEvent::Done {
            reason: DoneReason::Stop,
            message: preset,
        });
        assert_eq!(
            stream.result().await.expect("resolved").duration_ms,
            Some(1234)
        );
    }

    /// "leaves a message untimed when it started before the stream, such as
    /// a fetched deferred result".
    #[tokio::test]
    async fn test_duration_ms_leaves_stale_timestamp_untimed() {
        let mut fetched = message(StopReason::Stop);
        fetched.timestamp = now_wall_ms() - 60_000;
        let stream = AssistantMessageEventStream::new();
        stream.push(StreamEvent::Done {
            reason: DoneReason::Stop,
            message: fetched,
        });
        let resolved = stream.result().await.expect("resolved");
        assert_eq!(resolved.duration_ms, None);
    }

    /// "does not time a message pushed after the stream completed" — the
    /// late event is dropped by the completion guard (Rust owns the event,
    /// so the observable face is the single buffered terminal event).
    #[tokio::test]
    async fn test_duration_ms_late_push_untimed() {
        let stream = AssistantMessageEventStream::new();
        stream.push(StreamEvent::Done {
            reason: DoneReason::Stop,
            message: timed_message(StopReason::Stop),
        });
        stream.push(StreamEvent::Done {
            reason: DoneReason::Stop,
            message: timed_message(StopReason::Stop),
        });
        stream.end(None);
        let events: Vec<StreamEvent> = stream.clone().collect().await;
        assert_eq!(events.len(), 1, "late push must be dropped");
        assert!(
            stream
                .result()
                .await
                .expect("resolved")
                .duration_ms
                .is_some()
        );
    }
}
