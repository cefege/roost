//! The production driver that makes cells flow: v2's `queueMicrotask`/
//! `setTimeout` cadence around `session-cell-scheduler.ts`, `session-sync-output.ts`,
//! `session-raw-metadata.ts` and `session-terminal-metadata.ts`, plus `main.ts:200-217`
//! (the `"coord"` sink registration) and the session half of `transport/
//! coord-link-deps.ts:110-181` ([`LinkLifecyclePort`]). The lead calls
//! [`CellCadence::spawn`] once at boot and puts a clone in `DownstreamOwners.lifecycle`.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use roost_observability::clock::EventClock;
use roost_protocol::wire::brand::ChannelId;
use roost_protocol::wire::control::DIR_FROM_PTY;
use roost_protocol::wire::coord_worker::{Binary, CoordWorkerUpstream};

use crate::link_ports::LinkLifecyclePort;
use crate::session::cell_sink::{COORD_CELL_SINK_ID, CellSink};
use crate::session::emit::CellEmitter;
use crate::session::lifecycle::SessionTable;
use crate::session::raw_metadata::RawSend;
use crate::session::terminal_metadata::MetadataSend;
use crate::uplink::Uplink;

use super::link_loop::CoordinatorCellSink;

/// The emitter's timers, and the coordinator link's session-side lifecycle.
#[derive(Clone)]
pub struct CellCadence {
    shared: Arc<CadenceShared>,
}

struct CadenceShared {
    emitter: Arc<Mutex<CellEmitter>>,
    table: Arc<SessionTable>,
    clock: Arc<dyn EventClock>,
    uplink: Uplink,
    coord_sink: Arc<CoordinatorCellSink>,
    wake: Arc<tokio::sync::Notify>,
}

impl std::fmt::Debug for CellCadence {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CellCadence")
            .field("coord_sink", &self.shared.coord_sink)
            .finish_non_exhaustive()
    }
}

impl CellCadence {
    /// Build the cadence over the ONE emitter (`TableCellDelivery::emitter`),
    /// register `coord_sink` (the same `Arc` the link drains) the way v2's
    /// `main.ts` did, and start the driver task on the current runtime.
    pub fn spawn(
        emitter: Arc<Mutex<CellEmitter>>,
        table: Arc<SessionTable>,
        clock: Arc<dyn EventClock>,
        uplink: Uplink,
        coord_sink: Arc<CoordinatorCellSink>,
    ) -> (Self, tokio::task::JoinHandle<()>) {
        let cadence = Self::new(emitter, table, clock, uplink, coord_sink);
        let driver = cadence.clone();
        let handle = tokio::spawn(async move { driver.drive().await });
        tracing::info!("the cell cadence is running");
        (cadence, handle)
    }

    /// The cadence without its task: [`CellCadence::run_pass`] is then the only
    /// thing that runs it.
    pub fn new(
        emitter: Arc<Mutex<CellEmitter>>,
        table: Arc<SessionTable>,
        clock: Arc<dyn EventClock>,
        uplink: Uplink,
        coord_sink: Arc<CoordinatorCellSink>,
    ) -> Self {
        let wake = {
            let mut held = lock(&emitter);
            held.register_cell_sink(Arc::clone(&coord_sink) as Arc<dyn CellSink>);
            held.cadence_wake()
        };
        Self {
            shared: Arc::new(CadenceShared {
                emitter,
                table,
                clock,
                uplink,
                coord_sink,
                wake,
            }),
        }
    }

    /// v2 `registerCellSink` for a local terminal socket (or any other sink).
    pub fn register_sink(&self, sink: Arc<dyn CellSink>) {
        lock(&self.shared.emitter).register_cell_sink(sink);
    }

    /// v2 `unregisterCellSink`.
    pub fn unregister_sink(&self, sink_id: &str) {
        lock(&self.shared.emitter).unregister_cell_sink(sink_id);
    }

    /// The driver: one pass per wake or deadline, forever.
    async fn drive(self) {
        loop {
            let next = self.run_pass();
            match next {
                Some(deadline) if deadline <= Instant::now() => tokio::task::yield_now().await,
                Some(deadline) => {
                    tokio::select! {
                        () = self.shared.wake.notified() => {}
                        () = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {}
                    }
                }
                None => self.shared.wake.notified().await,
            }
        }
    }

    /// Run everything the emitter owes right now; returns when to look again
    /// (an instant not after now means "immediately").
    pub fn run_pass(&self) -> Option<Instant> {
        let now = Instant::now();
        let work = lock(&self.shared.emitter).cadence_work(now);
        for channel_id in &work.channels {
            let record = u16::try_from(channel_id.as_u32())
                .ok()
                .and_then(|raw| self.shared.table.record_of_channel(raw));
            let Some(record) = record else {
                lock(&self.shared.emitter).drop_cadence_work(*channel_id, now);
                continue;
            };
            // Record first, then emitter: the order every ingest takes.
            let mut record = record
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let now_ms = self.shared.clock.now_epoch_ms();
            lock(&self.shared.emitter).run_cadence_work(&mut record, now_ms, now);
        }
        if work.raw_due {
            self.dispatch_raw_metadata(now);
        }
        if work.metadata_due {
            self.flush_terminal_metadata();
        }
        let after = lock(&self.shared.emitter).cadence_work(Instant::now());
        if !after.channels.is_empty() || after.raw_due || after.metadata_due {
            return Some(now);
        }
        after.next_deadline
    }

    /// v2 `drainRawMetadata` into the link as `WBinary` (`DIR_FROM_PTY`).
    fn dispatch_raw_metadata(&self, now: Instant) {
        let live = self.live_channels();
        let uplink = &self.shared.uplink;
        let sent = lock(&self.shared.emitter).raw.dispatch(
            &|channel_id| live.contains(&channel_id),
            &mut |frame| {
                let binary = Binary {
                    channel_id: frame.channel_id,
                    direction: DIR_FROM_PTY,
                    data: frame.bytes.clone(),
                    seq: frame.end_seq,
                };
                if uplink.send(CoordWorkerUpstream::Binary(binary)) {
                    RawSend::Accepted
                } else {
                    RawSend::Dropped
                }
            },
            now,
        );
        tracing::trace!(sent, "raw metadata dispatched");
    }

    /// v2 `flushTerminalMetadata` into the link's coalescing lane.
    fn flush_terminal_metadata(&self) {
        let live = self.live_channels();
        let uplink = &self.shared.uplink;
        let now_ms = self.shared.clock.now_epoch_ms();
        lock(&self.shared.emitter).metadata.flush(
            &|channel_id| live.contains(&channel_id),
            &mut |metadata| {
                if uplink.send(CoordWorkerUpstream::TerminalMetadata(metadata)) {
                    MetadataSend::Accepted
                } else {
                    MetadataSend::Dropped
                }
            },
            now_ms,
        );
    }

    /// v2 `sessions.has`, taken before the emitter lock.
    fn live_channels(&self) -> HashSet<ChannelId> {
        self.shared
            .table
            .live()
            .into_iter()
            .filter_map(|(_, raw)| ChannelId::try_from(i64::from(raw)).ok())
            .collect()
    }

    /// Detach the coordinator sink from a socket generation that is gone or not
    /// yet acknowledged (v2 `setTerminalMetadataNegotiated(false)` + suspend).
    fn suspend_coordinator(&self, why: &str) {
        let mut emitter = lock(&self.shared.emitter);
        emitter.set_terminal_metadata_negotiated(false);
        emitter.suspend_cell_sink(COORD_CELL_SINK_ID);
        drop(emitter);
        self.shared.coord_sink.set_attached(false);
        let discarded = self.shared.coord_sink.discard_queued();
        tracing::info!(why, discarded, "the coordinator cell sink is suspended");
    }
}

impl LinkLifecyclePort for CellCadence {
    /// v2 `onOpen`: a fresh socket cannot carry cells until hello-ack.
    fn on_open(&self) {
        self.suspend_coordinator("open");
    }

    /// v2 `onHelloAck` (session half): negotiate, then re-baseline the coord
    /// sink ONLY — stream identity and local viewers are untouched.
    fn on_hello_ack(&self, terminal_metadata_negotiated: bool) {
        self.shared.coord_sink.set_attached(true);
        let mut emitter = lock(&self.shared.emitter);
        emitter.set_terminal_metadata_negotiated(terminal_metadata_negotiated);
        emitter.resume_cell_sink(COORD_CELL_SINK_ID);
        tracing::info!(
            terminal_metadata_negotiated,
            "the coordinator cell sink resumed at hello-ack"
        );
    }

    /// v2 `onDetach` (session half).
    fn on_detach(&self) {
        self.suspend_coordinator("detach");
    }

    /// v2 `onWritable`: resume parked parts, then retry coalesced metadata.
    /// Resuming drains the parked repair into the sink before this returns, so
    /// the link writes it ahead of its queued controls
    /// (`coord-link-repair-order.test.ts`).
    fn on_writable(&self) {
        lock(&self.shared.emitter).resume_cell_sink(COORD_CELL_SINK_ID);
        self.flush_terminal_metadata();
        tracing::debug!("the coordinator link reported writable");
    }

    /// v2 `onSnapshotReady` (session half): replay retained metadata, and ship a
    /// baseline parked at hello-ack now that the barrier cleared.
    fn on_snapshot_ready(&self) {
        let mut emitter = lock(&self.shared.emitter);
        emitter.replay_terminal_metadata();
        emitter.resume_cell_sink(COORD_CELL_SINK_ID);
        tracing::info!("the coordinator snapshot is live; metadata replayed and cells resumed");
    }
}

fn lock(emitter: &Mutex<CellEmitter>) -> MutexGuard<'_, CellEmitter> {
    emitter
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
