//! The one way anything other than the link loop puts a frame on the
//! coordinator link, the connection fence a reply is held to, and the request
//! budget a terminal-control owner works against. Sent into by
//! `runtime::downstream` owners and `runtime::link_loop::browser`; drained by
//! `runtime::link_serve`. Ports `activeSocket() === socket` / `isCurrent` and
//! `terminalBudget` from v2 `apps/worker/src/transport/coord-link-downstream.ts`,
//! `coord-link-direct-terminal.ts`, and the two caps in `coord-link-constants.ts`.

pub mod terminal_results;

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use roost_protocol::wire::coord_worker::CoordWorkerUpstream;
use tokio::sync::mpsc;

/// The work an owner hands back for a request it accepted synchronously.
pub type OwnerFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// Ceiling on the coordinator's relative `budget_ms`, and the budget used when
/// it sends none, so one request can never park an admission slot for ever.
/// v2 `coord-link-constants.ts` `TERMINAL_REQUEST_BUDGET_CAP_MS`.
pub const TERMINAL_REQUEST_BUDGET_CAP_MS: u32 = 30_000;

/// How many terminal-stream requests may be in flight at once. Stream state has
/// its own admission so resize traffic cannot block its own completion path.
/// v2 `coord-link-constants.ts` `TERMINAL_STREAM_REQUEST_INFLIGHT_CAP`.
pub const TERMINAL_STREAM_REQUEST_INFLIGHT_CAP: usize = 64;

/// Which coordinator connection a request arrived on.
///
/// `is_current()` is v2's `outbox.activeSocket() === socket`: it turns false
/// the moment the link loop re-dials or detaches, so a reply computed for a
/// superseded connection never reaches the replacement one.
#[derive(Clone, Debug)]
pub struct LinkFence {
    generation: u64,
    current: Arc<AtomicU64>,
}

impl LinkFence {
    pub fn is_current(&self) -> bool {
        self.current.load(Ordering::Acquire) == self.generation
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
}

/// v2 `TerminalRequestBudget`: a monotonic budget whose origin is frame
/// receipt, so time a request queued inside the worker is charged against the
/// same budget the coordinator is still waiting on, and neither host's wall
/// clock participates.
#[derive(Clone, Copy, Debug)]
pub struct RequestBudget {
    received: Instant,
    allowed: Duration,
}

impl RequestBudget {
    /// `budget_ms` is RELATIVE. Zero means "none sent" and gets the cap; a
    /// value above the cap is clamped to it.
    pub fn from_budget_ms(budget_ms: u32, received: Instant) -> Self {
        let allowed_ms = if budget_ms > 0 {
            budget_ms.min(TERMINAL_REQUEST_BUDGET_CAP_MS)
        } else {
            TERMINAL_REQUEST_BUDGET_CAP_MS
        };
        Self {
            received,
            allowed: Duration::from_millis(u64::from(allowed_ms)),
        }
    }

    pub fn remaining(&self, now: Instant) -> Duration {
        self.allowed
            .saturating_sub(now.saturating_duration_since(self.received))
    }

    pub fn expired(&self, now: Instant) -> bool {
        self.remaining(now).is_zero()
    }
}

/// One frame on its way to the link loop, with the connection it answers.
#[derive(Debug)]
struct RoutedFrame {
    frame: CoordWorkerUpstream,
    /// `None` for an unfenced send (v2 `link.send`), which waits for whichever
    /// connection is next.
    fence: Option<u64>,
    admission: Option<tokio::sync::oneshot::Sender<bool>>,
}

/// A frame received by the link loop, with an optional waiter for actual
/// outbox admission.
#[derive(Debug)]
pub struct UplinkDelivery {
    pub frame: CoordWorkerUpstream,
    admission: Option<tokio::sync::oneshot::Sender<bool>>,
    fence: Option<u64>,
    current: Arc<AtomicU64>,
}

impl UplinkDelivery {
    pub fn admit(self, admit: impl FnOnce(CoordWorkerUpstream) -> bool) {
        if self
            .fence
            .is_some_and(|fence| self.current.load(Ordering::Acquire) != fence)
        {
            if let Some(admission) = self.admission {
                let _ = admission.send(false);
            }
            return;
        }
        if self
            .admission
            .as_ref()
            .is_some_and(|admission| admission.is_closed())
        {
            return;
        }
        let admitted = admit(self.frame);
        if let Some(admission) = self.admission {
            let _ = admission.send(admitted);
        }
    }
    /// Extracts a fire-and-forget frame for diagnostics without discarding an
    /// admission waiter.
    pub fn _take_unconfirmed_frame(self) -> Option<CoordWorkerUpstream> {
        self.admission.is_none().then_some(self.frame)
    }
}

/// A frame could not be admitted on the coordinator connection current at call
/// time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the frame was not admitted on the current coordinator connection")]
pub struct UplinkAdmissionError;

/// The sending half. Clone + Send + Sync; every clone shares one generation.
#[derive(Clone, Debug)]
pub struct Uplink {
    sender: Option<mpsc::UnboundedSender<RoutedFrame>>,
    current: Arc<AtomicU64>,
    connected: Arc<std::sync::atomic::AtomicBool>,
    generation_changed: tokio::sync::watch::Receiver<u64>,
}

impl Uplink {
    /// A sender with no link loop behind it, for a test that never sends.
    /// Every send returns false.
    pub fn detached() -> Self {
        let (_, generation_changed) = tokio::sync::watch::channel(0);
        Self {
            sender: None,
            current: Arc::new(AtomicU64::new(0)),
            connected: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            generation_changed,
        }
    }
    /// Hand a frame to the link loop for whichever connection carries it.
    pub fn send(&self, frame: CoordWorkerUpstream) -> bool {
        self.route(RoutedFrame {
            frame,
            fence: None,
            admission: None,
        })
    }

    /// Wait for admission on the coordinator connection current at call time.
    pub async fn send_confirmed(
        &self,
        frame: CoordWorkerUpstream,
    ) -> Result<(), UplinkAdmissionError> {
        let mut generation_changed = self.generation_changed.clone();
        let generation = *generation_changed.borrow_and_update();
        if !self.connected.load(Ordering::Acquire)
            || self.current.load(Ordering::Acquire) != generation
        {
            return Err(UplinkAdmissionError);
        }
        let (admission, admitted) = tokio::sync::oneshot::channel();
        if !self.route(RoutedFrame {
            frame,
            fence: Some(generation),
            admission: Some(admission),
        }) {
            return Err(UplinkAdmissionError);
        }
        tokio::select! {
            biased;
            result = admitted => result.map_err(|_| UplinkAdmissionError)?.then_some(()).ok_or(UplinkAdmissionError),
            changed = generation_changed.changed() => {
                let _ = changed;
                Err(UplinkAdmissionError)
            }
        }
    }
    /// Hand a reply to the link loop only while its connection is current.
    /// A stale fence drops the frame here, and the link loop checks again when
    /// it admits, because the link can re-dial while the frame is in the
    /// channel.
    pub fn send_fenced(&self, fence: &LinkFence, frame: CoordWorkerUpstream) -> bool {
        if !fence.is_current() {
            tracing::debug!(
                kind = frame.kind(),
                fence = fence.generation,
                "a reply for a superseded coordinator connection was dropped"
            );
            return false;
        }
        self.route(RoutedFrame {
            frame,
            fence: Some(fence.generation),
            admission: None,
        })
    }

    /// The fence of the connection that is current right now.
    pub fn fence(&self) -> LinkFence {
        LinkFence {
            generation: self.current.load(Ordering::Acquire),
            current: Arc::clone(&self.current),
        }
    }

    fn route(&self, routed: RoutedFrame) -> bool {
        let Some(sender) = &self.sender else {
            tracing::debug!(
                kind = routed.frame.kind(),
                "a frame was offered to a detached uplink"
            );
            return false;
        };
        sender.send(routed).is_ok()
    }
}

/// The receiving half, owned by the link loop. It holds a sender of its own,
/// so the channel never reports closed while the link loop exists.
#[derive(Debug)]
pub struct UplinkReceiver {
    receiver: mpsc::UnboundedReceiver<RoutedFrame>,
    sender: mpsc::UnboundedSender<RoutedFrame>,
    current: Arc<AtomicU64>,
    connected: Arc<std::sync::atomic::AtomicBool>,
    generation_changed: tokio::sync::watch::Sender<u64>,
}

/// A connected pair: the composition root keeps the [`Uplink`] (cloning it to
/// every producer) and hands the [`UplinkReceiver`] to `LinkLoop::new`.
pub fn channel() -> (Uplink, UplinkReceiver) {
    let (sender, receiver) = mpsc::unbounded_channel();
    let current = Arc::new(AtomicU64::new(0));
    let connected = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (generation_changed, generation_updates) = tokio::sync::watch::channel(0);
    let uplink = Uplink {
        sender: Some(sender.clone()),
        current: Arc::clone(&current),
        connected: Arc::clone(&connected),
        generation_changed: generation_updates,
    };
    (
        uplink,
        UplinkReceiver {
            receiver,
            sender,
            current,
            connected,
            generation_changed,
        },
    )
}

impl UplinkReceiver {
    /// Another sender on this channel, for the link loop's own dispatcher.
    pub fn uplink(&self) -> Uplink {
        Uplink {
            sender: Some(self.sender.clone()),
            current: Arc::clone(&self.current),
            connected: Arc::clone(&self.connected),
            generation_changed: self.generation_changed.subscribe(),
        }
    }

    pub async fn recv_delivery(&mut self) -> Option<UplinkDelivery> {
        loop {
            let routed = self.receiver.recv().await?;
            if let Some(delivery) = self.admit(routed) {
                return Some(delivery);
            }
        }
    }

    pub fn try_recv_delivery(&mut self) -> Option<UplinkDelivery> {
        while let Ok(routed) = self.receiver.try_recv() {
            if let Some(delivery) = self.admit(routed) {
                return Some(delivery);
            }
        }
        None
    }

    /// A connection opened or ended: every fence taken before this is stale.
    pub fn advance(&self) -> u64 {
        let generation = self.current.fetch_add(1, Ordering::AcqRel) + 1;
        self.generation_changed.send_replace(generation);
        tracing::debug!(generation, "the coordinator connection generation advanced");
        generation
    }

    pub fn link_opened(&self) -> u64 {
        self.connected.store(true, Ordering::Release);
        self.advance()
    }

    pub fn link_detached(&self) -> u64 {
        self.connected.store(false, Ordering::Release);
        self.advance()
    }

    pub fn generation(&self) -> u64 {
        self.current.load(Ordering::Acquire)
    }

    fn admit(&self, routed: RoutedFrame) -> Option<UplinkDelivery> {
        let RoutedFrame {
            frame,
            fence,
            admission,
        } = routed;
        match fence {
            Some(fence) if fence != self.generation() => {
                tracing::debug!(
                    kind = frame.kind(),
                    fence,
                    generation = self.generation(),
                    "a reply outlived its coordinator connection and was dropped"
                );
                if let Some(admission) = admission {
                    let _ = admission.send(false);
                }
                None
            }
            _ => Some(UplinkDelivery {
                frame,
                admission,
                fence,
                current: Arc::clone(&self.current),
            }),
        }
    }
}
