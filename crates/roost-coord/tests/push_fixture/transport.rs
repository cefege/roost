//! The push tests' transport doubles: a recording transport and a counting
//! VAPID generator.
//!
//! Owned by the push tests. The recording transport is what lets a test assert
//! on the two rules that are easy to lose in a tidier port -- that only 404 and
//! 410 prune, and that no more than four sends overlap -- without a network.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use roost_coord::push::transport::{
    PushDeliveryRequest, PushNotificationTransport, PushTransportError,
};
use roost_coord::push::vapid::{P256KeypairGenerator, VapidError, VapidKeyGenerator, VapidKeys};

/// One delivery the fake transport saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedDelivery {
    /// The endpoint the delivery was addressed to.
    pub endpoint: String,
    /// The payload bytes, as text.
    pub body: String,
    /// The RFC 8030 topic, when the dispatch set one.
    pub topic: Option<String>,
    /// The TTL the request carried, in seconds.
    pub ttl_secs: u64,
    /// The per-attempt ceiling the request carried, in milliseconds.
    pub timeout_ms: u128,
}

/// A transport that records what it was asked to send and answers from a
/// script.
pub struct FakeTransport {
    deliveries: Mutex<Vec<RecordedDelivery>>,
    in_flight: AtomicUsize,
    peak_in_flight: AtomicUsize,
    /// The gate every attempt parks on, when this transport holds.
    gate: Option<tokio::sync::watch::Receiver<bool>>,
    /// The status to fail with. Every attempt fails the same way, which is
    /// what the pruning and the isolation tests each need.
    fail_with: Option<PushTransportError>,
}

impl FakeTransport {
    /// A transport that accepts everything.
    #[must_use]
    pub fn accepting() -> Arc<Self> {
        Arc::new(Self::new(None, None))
    }

    /// A transport that fails every attempt with `error`.
    #[must_use]
    pub fn failing(error: PushTransportError) -> Arc<Self> {
        Arc::new(Self::new(Some(error), None))
    }

    /// A transport that parks every attempt until the returned handle says go.
    ///
    /// A test drives it by setting the watch to `true`; until then the attempts
    /// sit in flight, which is how the concurrency ceiling becomes observable
    /// without a sleep.
    #[must_use]
    pub fn gated() -> (Arc<Self>, tokio::sync::watch::Sender<bool>) {
        let (sender, receiver) = tokio::sync::watch::channel(false);
        (Arc::new(Self::new(None, Some(receiver))), sender)
    }

    fn new(
        fail_with: Option<PushTransportError>,
        gate: Option<tokio::sync::watch::Receiver<bool>>,
    ) -> Self {
        Self {
            deliveries: Mutex::new(Vec::new()),
            in_flight: AtomicUsize::new(0),
            peak_in_flight: AtomicUsize::new(0),
            gate,
            fail_with,
        }
    }

    /// Everything the transport was asked to send, in completion order.
    #[must_use]
    pub fn deliveries(&self) -> Vec<RecordedDelivery> {
        self.deliveries.lock().expect("the delivery log").clone()
    }

    /// How many attempts overlapped at the busiest moment.
    #[must_use]
    pub fn peak_in_flight(&self) -> usize {
        self.peak_in_flight.load(Ordering::SeqCst)
    }

    /// How many attempts have been recorded.
    #[must_use]
    pub fn attempted(&self) -> usize {
        self.deliveries.lock().expect("the delivery log").len()
    }

    /// Park on the gate, if this transport has one.
    async fn enter(&self) {
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak_in_flight.fetch_max(now, Ordering::SeqCst);
        if let Some(gate) = &self.gate {
            let mut gate = gate.clone();
            while !*gate.borrow_and_update() {
                if gate.changed().await.is_err() {
                    // The sender is gone, which means "stop holding".
                    return;
                }
            }
        }
    }
}

#[async_trait::async_trait]
impl PushNotificationTransport for FakeTransport {
    async fn send(&self, request: &PushDeliveryRequest) -> Result<(), PushTransportError> {
        self.enter().await;
        self.deliveries
            .lock()
            .expect("the delivery log")
            .push(RecordedDelivery {
                endpoint: request.endpoint.clone(),
                body: request.body.clone(),
                topic: request.topic.clone(),
                ttl_secs: request.ttl.as_secs(),
                timeout_ms: request.timeout.as_millis(),
            });
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        match &self.fail_with {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }
}

/// A VAPID generator that counts draws and mints a distinct identity each time.
///
/// The count is what makes "first use serialises" observable. A test that only
/// asserted one row exists would pass against a coordinator that minted eight
/// identities and kept one.
pub struct CountingGenerator {
    draws: Arc<AtomicUsize>,
}

impl CountingGenerator {
    /// A generator and the counter that records how many times it ran.
    #[must_use]
    pub fn new() -> (Arc<Self>, Arc<AtomicUsize>) {
        let counter = Arc::new(AtomicUsize::new(0));
        (
            Arc::new(Self {
                draws: Arc::clone(&counter),
            }),
            counter,
        )
    }
}

impl VapidKeyGenerator for CountingGenerator {
    fn generate(&self) -> Result<VapidKeys, VapidError> {
        self.draws.fetch_add(1, Ordering::SeqCst);
        P256KeypairGenerator.generate()
    }
}
