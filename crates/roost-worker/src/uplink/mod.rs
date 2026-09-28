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
}

/// The sending half. Clone + Send + Sync; every clone shares one generation.
#[derive(Clone, Debug)]
pub struct Uplink {
    sender: Option<mpsc::UnboundedSender<RoutedFrame>>,
    current: Arc<AtomicU64>,
}

impl Uplink {
    /// A sender with no link loop behind it, for a test that never sends.
    /// Every send returns false.
    pub fn detached() -> Self {
        Self {
            sender: None,
            current: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Hand a frame to the link loop for whichever connection carries it.
    pub fn send(&self, frame: CoordWorkerUpstream) -> bool {
        self.route(RoutedFrame { frame, fence: None })
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
}

/// A connected pair: the composition root keeps the [`Uplink`] (cloning it to
/// every producer) and hands the [`UplinkReceiver`] to `LinkLoop::new`.
pub fn channel() -> (Uplink, UplinkReceiver) {
    let (sender, receiver) = mpsc::unbounded_channel();
    let current = Arc::new(AtomicU64::new(0));
    let uplink = Uplink {
        sender: Some(sender.clone()),
        current: Arc::clone(&current),
    };
    (
        uplink,
        UplinkReceiver {
            receiver,
            sender,
            current,
        },
    )
}

impl UplinkReceiver {
    /// Another sender on this channel, for the link loop's own dispatcher.
    pub fn uplink(&self) -> Uplink {
        Uplink {
            sender: Some(self.sender.clone()),
            current: Arc::clone(&self.current),
        }
    }

    /// The next frame whose connection is still current. Cancel-safe: a frame
    /// skipped here was stale and is dropped on purpose.
    pub async fn recv(&mut self) -> Option<CoordWorkerUpstream> {
        loop {
            let routed = self.receiver.recv().await?;
            if let Some(frame) = self.admit(routed) {
                return Some(frame);
            }
        }
    }

    /// [`UplinkReceiver::recv`] without waiting.
    pub fn try_recv(&mut self) -> Option<CoordWorkerUpstream> {
        while let Ok(routed) = self.receiver.try_recv() {
            if let Some(frame) = self.admit(routed) {
                return Some(frame);
            }
        }
        None
    }

    /// A connection opened or ended: every fence taken before this is stale.
    pub fn advance(&self) -> u64 {
        let generation = self.current.fetch_add(1, Ordering::AcqRel) + 1;
        tracing::debug!(generation, "the coordinator connection generation advanced");
        generation
    }

    pub fn generation(&self) -> u64 {
        self.current.load(Ordering::Acquire)
    }

    fn admit(&self, routed: RoutedFrame) -> Option<CoordWorkerUpstream> {
        match routed.fence {
            Some(fence) if fence != self.generation() => {
                tracing::debug!(
                    kind = routed.frame.kind(),
                    fence,
                    generation = self.generation(),
                    "a reply outlived its coordinator connection and was dropped"
                );
                None
            }
            _ => Some(routed.frame),
        }
    }
}
