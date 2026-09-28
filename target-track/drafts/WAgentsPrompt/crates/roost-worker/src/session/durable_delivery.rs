//! The one signal between the durable outbox's writer and the coordinator
//! link that replays it: "the store changed" (a row appended, a claim taken,
//! held or given back) and "every durable row has reached the coordinator".
//! Raised by `session::journal_sink::JournalSink`; drained by the link loop
//! (`runtime::link_loop::durable_sync`); awaited by the boot reconcile before it
//! reads recovery metadata. Ports the `link.send` / `snapshotStateChanged`
//! edges of v2 `apps/worker/src/transport/event-sink.ts` and
//! `apps/worker/src/transport/coord-link-replay-barrier.ts`.

use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::{Notify, watch};

/// Shared by the durable sink and the link. Every method is `&self`.
#[derive(Debug)]
pub struct DurableDelivery {
    /// Set by the sink, taken by the link: the store holds something the link
    /// has not yet read.
    changed: AtomicBool,
    wake: Notify,
    /// v2 `DurableSessionEventReplayBarrier`: `true` once nothing durable is
    /// left to replay and no claim blocks the snapshot.
    drained: watch::Sender<bool>,
}

impl Default for DurableDelivery {
    fn default() -> Self {
        Self {
            changed: AtomicBool::new(false),
            wake: Notify::new(),
            drained: watch::Sender::new(false),
        }
    }
}

impl DurableDelivery {
    pub fn new() -> Self {
        Self::default()
    }

    /// The store changed. The replay is pending again until the link has read
    /// the change and found nothing left to deliver: v2 marks the barrier
    /// pending synchronously with the append, so a waiter can never read
    /// "drained" over a row it was not told about.
    pub fn note_store_changed(&self) {
        self.changed.store(true, Ordering::Release);
        self.mark_pending();
        self.wake.notify_one();
    }

    /// Whether the store changed since the link last looked, clearing it.
    pub fn take_store_changed(&self) -> bool {
        self.changed.swap(false, Ordering::AcqRel)
    }

    /// Resolves when the sink has raised a change the link has not taken.
    pub async fn store_changed(&self) {
        self.wake.notified().await;
    }

    pub fn mark_pending(&self) {
        self.drained.send_if_modified(|drained| std::mem::replace(drained, false));
    }

    pub fn mark_drained(&self) {
        let changed = self.drained.send_if_modified(|drained| !std::mem::replace(drained, true));
        if changed {
            tracing::debug!("every durable session event has reached the coordinator");
        }
    }

    pub fn is_drained(&self) -> bool {
        *self.drained.borrow()
    }

    /// v2 `waitForDurableSessionEventReplay`: returns once the link has replayed
    /// every durable row and no claim blocks the snapshot. Survives a
    /// disconnect while pending; a caller that must give up drops the future.
    pub async fn wait_for_replay(&self) {
        let mut drained = self.drained.subscribe();
        // `wait_for` only fails once the sender is gone, and the sender lives in
        // `self`, which this borrow keeps alive.
        let _ = drained.wait_for(|drained| *drained).await;
    }
}
