//! The keeper pool: every live PTY on this machine, one connection, one table.
//! `session::spawn` opens channels through it, `session::sinks::ChannelBinding`
//! receives their bytes, and the boot reconcile re-adopts survivors through it.
//! Depends on `runtime::keeper_boot::KeeperHandle` for the connection,
//! `roost_keeper::client` for every request, and `super::channel_ids` for the
//! one channel-id fact it refuses against. Opening the PTY itself is
//! `super::pool_spawn`; and this file holds NO allocator, which is the point.
//!
//! IT OWNS NO KEEPER LIFECYCLE, AND NO CHANNEL-ID COUNTER EITHER. Whether this
//! worker adopts a survivor or replaces it is `runtime::keeper_boot`'s
//! decision; the counter that mints a fresh id belongs to
//! `session::lifecycle::SessionManager`, beside the stray reaper that advances
//! it past the keeper's own maximum. This pool used to hold a SECOND copy of
//! that counter and advance it from `adopt`, which meant the worker held two
//! allocators of one id space and only one of them was the one callers used.
//! What is left here is [`ChannelIds`], which mints nothing and refuses an id
//! the keeper is known to hold — a caller that skipped the allocator cannot
//! hand this keeper a channel it already owns.
//!
//! ONE LOCK, AND WHY IT IS THE SAME ONE TWICE. The connection handle serialises
//! every request, and it is also what keeps the dispatch loop from stealing a
//! frame a request is waiting for: the client hands a request's answer to the
//! waiting caller, and only frames nobody claimed reach the pool's event stream.
//! So a request holding the handle proves no dispatch is draining, and every
//! answer the dispatcher sees afterwards belongs to a request that already
//! returned. That is also what keeps a resize's result ahead of the PTY bytes
//! the resize produced: the result is settled inside the request, and no later
//! frame is dispatched until the handle is free.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;

use roost_keeper::client::KeeperClient;
use roost_keeper::client_error::ClientError;
use roost_keeper::client_frames::EventPoll;
use roost_keeper::client_resize::{ResizeOutcome, ResizeUnknownReason};
use roost_keeper::codec::{KEEPER_MAX_INPUT_BYTES, MuxFrame};
use roost_keeper::frames::ChannelBinding as KeeperChannelBinding;

use super::PoolChannel;
use super::channel_ids::ChannelIds;
use super::channels::ChannelRegistry;
use super::dispatch::dispatch_loop;
use super::error::PoolError;
use super::input_command::{PendingInputUsage, PendingInputs};
use super::pending_resizes::PendingResizes;
use super::pool_lifecycle::KeeperDeathHook;
use crate::runtime::keeper_boot::KeeperHandle;
use crate::session::keeper_channels::{InputNotWritten, KeeperInputCommand};
use crate::session::sinks::ChannelBinding;

/// A PTY the keeper opened, and the channel it is addressed by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spawned {
    pub channel_id: u16,
    pub pid: u32,
}

/// Every live PTY on this machine, multiplexed over one keeper connection.
pub struct KeeperPool {
    /// `pub(super)` so `pool_spawn` can read the connection and the table it
    /// writes, and nothing outside `keeper_pool` can.
    pub(super) keeper: KeeperHandle,
    pub(super) channels: ChannelRegistry,
    /// The highest channel id the keeper is known to hold, so a spawn is
    /// refused an id this worker has already spent. Not an allocator: the
    /// counter that mints one is `SessionManager`'s, and a second copy of it
    /// here is what let a fresh worker and a surviving keeper disagree.
    pub(super) channel_ids: ChannelIds,
    /// Cleared the moment the keeper is known to be gone, so a later request
    /// fails instead of writing into a socket nobody is reading.
    pub(super) connected: AtomicBool,
    /// Written acknowledged-input batches awaiting their result frame.
    /// `pub(super)` so `dispatch` can settle them.
    pub(super) pending_inputs: Arc<PendingInputs>,
    /// Held by a dispatch pass from take to last delivery, and by a history
    /// read at its ordered boundary, so neither interleaves with the other.
    pub(super) routing: Mutex<()>,
    /// Resizes written and not yet answered (v2 `pendingResizes`).
    pub(super) pending_resizes: PendingResizes,
    /// What a lost connection fires once its channels are ended.
    pub(super) death_hook: Mutex<Option<KeeperDeathHook>>,
}

/// What one dispatch pass took off the connection, and whether the keeper's
/// side of it has closed.
pub(crate) struct ArrivedFrames {
    pub frames: Vec<MuxFrame>,
    pub closed: bool,
}

impl std::fmt::Debug for KeeperPool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KeeperPool")
            .field("connected", &self.connected.load(Ordering::SeqCst))
            .field("live_channels", &self.live_bindings().len())
            .field("spawning", &self.spawning_channels().len())
            .finish_non_exhaustive()
    }
}

impl KeeperPool {
    /// Take a connection and start delivering what the keeper sends.
    ///
    /// The dispatch loop starts here rather than on the first spawn because a
    /// PTY's output begins the moment its acknowledgement lands, and a loop
    /// that started with the first spawn would drop the bytes between them.
    pub fn new(keeper: KeeperHandle) -> Arc<Self> {
        let pool = Arc::new(Self {
            keeper,
            channels: ChannelRegistry::default(),
            channel_ids: ChannelIds::new(),
            connected: AtomicBool::new(true),
            pending_inputs: Arc::new(PendingInputs::default()),
            routing: Mutex::new(()),
            pending_resizes: PendingResizes::default(),
            death_hook: Mutex::new(None),
        });
        tracing::info!("the keeper pool is driving its connection");
        let weak: Weak<Self> = Arc::downgrade(&pool);
        if let Err(err) = std::thread::Builder::new()
            .name("roost-keeper-dispatch".into())
            .spawn(move || dispatch_loop(&weak))
        {
            tracing::error!(%err, "the keeper dispatch loop could not start");
        }
        pool
    }

    /// Take over a channel a previous worker or this keeper already owns.
    ///
    /// Registers nothing on the wire: the PTY exists, and the only thing this
    /// worker owes it is somewhere to deliver the bytes it is already sending.
    pub fn adopt(&self, channel_id: u16, pid: u32, output: Arc<dyn ChannelBinding>) {
        if self.channels.adopt(channel_id, pid, output) {
            tracing::warn!(
                channel_id,
                pid,
                "adopted over a channel already in this pool"
            );
        }
        // The id is now KNOWN to be the keeper's, which is what a later spawn
        // refuses against. Advancing the counter is `SessionManager`'s job and
        // happens once, against the keeper's WHOLE list, not per channel.
        self.channel_ids.note(channel_id);
        tracing::info!(channel_id, pid, "adopted a surviving channel");
    }

    /// Write input without waiting for an answer.
    ///
    /// The keystroke path of the legacy binary frame: nothing is owed back, so
    /// nothing correlates it. [`KeeperPool::begin_acknowledged_input`] is the
    /// form whose outcome a caller can report.
    pub fn input(&self, channel_id: u16, bytes: &[u8]) -> Result<(), PoolError> {
        self.require_connected()?;
        self.request(|client| client.write_input(channel_id, bytes))
    }

    /// Put one acknowledged batch on the socket under a worker-owned sequence,
    /// and hand back its two halves (v2 `beginInput`).
    ///
    /// The sequence is claimed, written and registered while this call holds
    /// the connection, and the dispatcher needs the connection to take frames,
    /// so no answer can arrive before its waiter exists. A write that failed
    /// registered nothing, which is what makes its refusal provable.
    pub fn begin_acknowledged_input(&self, channel_id: u16, bytes: Vec<u8>) -> KeeperInputCommand {
        if self.require_connected().is_err() {
            return KeeperInputCommand::not_written(InputNotWritten::Disconnected);
        }
        if bytes.is_empty() || bytes.len() > KEEPER_MAX_INPUT_BYTES as usize {
            return KeeperInputCommand::not_written(InputNotWritten::InvalidRequest);
        }
        let expected = bytes.len() as u32;
        let written: Result<KeeperInputCommand, String> = self.keeper.with(|client| {
            let input_seq = match self.pending_inputs.reserve(channel_id, bytes.len()) {
                Ok(input_seq) => input_seq,
                Err(refusal) => return Ok(KeeperInputCommand::not_written(refusal)),
            };
            match client.send_input_request(channel_id, input_seq, &bytes) {
                Ok(()) => Ok(self
                    .pending_inputs
                    .register(channel_id, input_seq, expected)),
                Err(error) => Err(error.to_string()),
            }
        });
        written.unwrap_or_else(|error| {
            self.keeper_lost(format!(
                "the keeper connection failed writing input: {error}"
            ));
            KeeperInputCommand::not_written(InputNotWritten::Disconnected)
        })
    }

    /// What a channel has written and not yet heard back about.
    pub fn pending_input(&self, channel_id: u16) -> PendingInputUsage {
        self.pending_inputs.usage(channel_id)
    }

    /// Apply a geometry change, and report what the keeper did with it.
    ///
    /// Acknowledged on purpose. A resize that is written and not confirmed is a
    /// terminal whose grid no longer matches the PTY it is painting, and the
    /// mismatch stays invisible until a full-screen program redraws at the old
    /// size.
    ///
    /// The outcome is handed back rather than collapsed: a refusal names why the
    /// PTY will not move, and an unknown is the one case the caller recovers by
    /// asking `resize_status` instead of resending. A `Disconnected` unknown is
    /// also the only signal that the connection went, since a resize's refusal
    /// and its silence are both answers rather than I/O errors now — so the
    /// loss is reported here, keeping this pool's rule that only a failed
    /// connection declares the keeper lost.
    pub fn resize(
        &self,
        channel_id: u16,
        seq: u64,
        cols: u16,
        rows: u16,
    ) -> Result<ResizeOutcome, PoolError> {
        self.require_connected()?;
        let Some(_pending) = self.pending_resizes.begin(channel_id, seq) else {
            tracing::warn!(channel_id, seq, "a resize under an in-flight sequence was refused before it was written");
            return Err(PoolError::ResizeInFlight { channel_id, seq });
        };
        tracing::debug!(channel_id, seq, cols, rows, "resizing a keeper channel");
        let outcome = self.request(|client| Ok(client.resize(channel_id, seq, cols, rows)))?;
        if let ResizeOutcome::Unknown {
            reason: ResizeUnknownReason::Disconnected,
            ..
        } = outcome
        {
            self.keeper_lost(format!(
                "the keeper connection failed during a resize of channel {channel_id}"
            ));
        }
        Ok(outcome)
    }

    /// When each resize still in flight on `channel_id` was written, for the
    /// pipeline's `KEEPER_RESIZE_PENDING` evidence.
    pub fn pending_resize_starts(&self, channel_id: u16) -> Vec<Instant> {
        self.pending_resizes.started(channel_id)
    }

    /// The channels the keeper says it still owns, for a reconcile.
    ///
    /// Reading the list is also how this pool LEARNS it. The only place the
    /// worker ever asks the keeper what it holds is this call, so teaching the
    /// guard here means a boot that reconciles needs no second wiring to know
    /// which ids are spoken for — and a caller cannot get a fresh id from the
    /// pool before the pool has been told what the keeper is holding.
    pub fn keeper_channels(&self) -> Result<Vec<KeeperChannelBinding>, PoolError> {
        self.require_connected()?;
        let channels = self.request(|client| client.list_channels().map(|list| list.channels))?;
        for channel in &channels {
            self.channel_ids.note(channel.channel_id);
        }
        Ok(channels)
    }

    /// The pairs a hello announces: live channels only.
    pub fn live_bindings(&self) -> Vec<KeeperChannelBinding> {
        self.channels.bindings()
    }

    /// The channels whose spawn is in flight.
    ///
    /// Published because "this keeper holds no channels" is false while one of
    /// these exists, and a boot that replaced a keeper mid-spawn would kill a
    /// terminal that was about to exist.
    pub fn spawning_channels(&self) -> Vec<u16> {
        self.channels.spawning()
    }

    /// Drop a channel from the pool entirely, for a session that has closed.
    pub fn forget(&self, channel_id: u16) -> Option<PoolChannel> {
        self.pending_inputs.forget_channel(channel_id);
        self.channels.forget(channel_id)
    }

    /// Whether this worker knows the channel, and has seen its child end.
    pub fn has_exited(&self, channel_id: u16) -> bool {
        self.channels.has_exited(channel_id)
    }

    /// Whether the pool still believes it has a keeper.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    /// Take every frame that has already arrived, off the connection.
    ///
    /// The connection handle is the discipline, and holding it is this
    /// method's whole job: the frame that answers a request is handed to the
    /// waiting caller inside the call holding this same handle, so draining
    /// under it is what keeps the dispatcher from taking a reply out from
    /// under the request blocked on it. The frames come back OWNED, so no lock
    /// is still held once a session is called.
    pub(crate) fn take_arrived_frames(&self) -> ArrivedFrames {
        self.keeper.with(|client| {
            let mut frames = Vec::new();
            loop {
                match client.poll_event() {
                    EventPoll::Frame(frame) => frames.push(frame),
                    EventPoll::Empty => return ArrivedFrames { frames, closed: false },
                    EventPoll::Closed => return ArrivedFrames { frames, closed: true },
                }
            }
        })
    }

    /// Where a channel's bytes go, or `None` when this worker does not drive it.
    ///
    /// The binding is CLONED out from under the table's lock and the lock is
    /// released before the caller touches it: a session that blocks inside
    /// `on_output` must not be able to stall the dispatcher for every other
    /// session, which is what holding that lock across the call would do.
    pub(crate) fn output_binding_for(&self, channel_id: u16) -> Option<Arc<dyn ChannelBinding>> {
        self.channels.output_for(channel_id)
    }

    /// Claim a channel's ending, for exactly one caller.
    ///
    /// The claim is taken HERE, under the table's lock, and only the winner is
    /// handed the binding: a connection that dies while an exit is in flight
    /// must not produce an exit AND an error for one channel, so the loser of
    /// that race is the one that finds the channel already gone.
    pub(crate) fn claim_channel_exit(&self, channel_id: u16) -> Option<Arc<dyn ChannelBinding>> {
        self.channels.claim_exit(channel_id)
    }

    /// Run one request, and treat a broken socket as a lost keeper.
    ///
    /// The write half is the only place a dead keeper is visible to a caller
    /// that is not reading the socket, so this is where the pool learns it: a
    /// silent success would leave every session believing its PTY still
    /// answers. Only a failed CONNECTION is treated as loss — a request the
    /// keeper simply did not answer in time is a wedged keeper, and declaring
    /// every session lost over one unanswered resize would take the machine's
    /// terminals away for a condition a retry fixes.
    ///
    /// `pub(crate)` rather than private because `session_seam` is a sibling
    /// that answers `KeeperChannels` from the same connection, and a second
    /// request path would be a second answer to "has this keeper gone".
    pub(crate) fn request<T>(
        &self,
        use_client: impl FnOnce(&KeeperClient) -> Result<T, ClientError>,
    ) -> Result<T, PoolError> {
        let outcome = self.keeper.with(use_client);
        if let Err(ClientError::Io(reason)) = &outcome {
            self.keeper_lost(format!("the keeper connection failed: {reason}"));
        }
        Ok(outcome?)
    }

    pub(super) fn require_connected(&self) -> Result<(), PoolError> {
        if self.connected.load(Ordering::SeqCst) {
            return Ok(());
        }
        Err(PoolError::Disconnected(
            "the keeper connection was reported gone".into(),
        ))
    }
}
