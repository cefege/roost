//! The keeper pool: every live PTY on this machine, one connection, one table.
//! `session::spawn` opens channels through it, `session::sinks::ChannelBinding`
//! receives their bytes, and the boot reconcile re-adopts survivors through it.
//! Depends on `runtime::keeper_boot::KeeperHandle` for the connection,
//! `roost_keeper::client` for every request, and `crate::strays` for the
//! channel-id allocator — nothing here.
//!
//! IT OWNS NO KEEPER LIFECYCLE. Whether this worker adopts a survivor or
//! replaces it is `runtime::keeper_boot`'s decision; this pool is handed a
//! connection and drives it. A pool that decided for itself would be a second
//! answer to "is it safe to replace the thing holding this machine's terminals".
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
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use roost_keeper::client::KeeperClient;
use roost_keeper::client_error::ClientError;
use roost_keeper::client_resize::{ResizeOutcome, ResizeUnknownReason};
use roost_keeper::frames::ChannelBinding as KeeperChannelBinding;
use roost_keeper::payloads::PtyInResult;

use super::PoolChannel;
use super::channels::ChannelRegistry;
use super::dispatch::dispatch_loop;
use super::error::PoolError;
use super::spawn_spec::pty_command;
use crate::runtime::keeper_boot::KeeperHandle;
use crate::session::sinks::ChannelBinding;
use crate::shell_spec::ShellSpec;
use crate::strays::ChannelAllocator;

/// How long the dispatch loop sleeps when the keeper said nothing.
///
/// The keeper's own output tick is 16ms, so this is the shortest sleep that
/// cannot outrun the producer. A tighter loop would burn a core per worker
/// doing nothing, which is what this number is for.
pub const DISPATCH_IDLE: Duration = Duration::from_millis(16);

/// A PTY the keeper opened, and the channel it is addressed by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spawned {
    pub channel_id: u16,
    pub pid: u32,
}

/// Every live PTY on this machine, multiplexed over one keeper connection.
pub struct KeeperPool {
    keeper: KeeperHandle,
    channels: ChannelRegistry,
    /// The worker-owned channel-id allocator. Behind its own lock because
    /// allocation is a counter bump while the connection lock is held across a
    /// request, and the two must not queue behind each other.
    allocator: Mutex<ChannelAllocator>,
    /// Cleared the moment the keeper is known to be gone, so a later request
    /// fails instead of writing into a socket nobody is reading.
    connected: AtomicBool,
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
            allocator: Mutex::new(ChannelAllocator::new()),
            connected: AtomicBool::new(true),
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

    /// Open a PTY running `spec`, delivering its output into `output`.
    ///
    /// The output binding is registered BEFORE the spawn frame is written, so
    /// the first bytes after the acknowledgement have a session to reach; and
    /// the id comes from the worker-owned allocator, so a fresh worker cannot
    /// collide with a channel the surviving keeper still holds.
    pub fn spawn(
        &self,
        spec: &ShellSpec,
        cols: u16,
        rows: u16,
        output: Arc<dyn ChannelBinding>,
    ) -> Result<Spawned, PoolError> {
        self.require_connected()?;
        let channel_id = self.take_channel_id()?;
        let command = pty_command(spec);
        if command.withheld_any() {
            // Reportable because it means a resolver handed this pool a spec
            // carrying a credential; the refusal itself is already done.
            tracing::warn!(
                channel_id,
                withheld = ?command.withheld,
                executable = %spec.executable,
                "withheld keeper control credentials from a PTY environment"
            );
        }
        if self.channels.begin_spawn(channel_id, output) {
            tracing::warn!(channel_id, "respawning a channel the keeper already owns");
        }
        match self
            .keeper
            .with(|client| client.spawn(channel_id, command.command, cols, rows))
        {
            Ok(pid) => {
                if let Err(err) = self.channels.finish_spawn(channel_id, pid) {
                    // The PTY is real; only the pool's record of it is gone. The
                    // strays reaper is the designed answer to a channel nobody
                    // tracks, and saying so beats a caller that believes it has
                    // no terminal and leaves a shell running.
                    tracing::error!(
                        channel_id,
                        pid,
                        %err,
                        "the keeper opened a channel this pool can no longer track"
                    );
                }
                tracing::info!(
                    channel_id,
                    pid,
                    executable = %spec.executable,
                    cwd = %spec.cwd,
                    "the keeper opened a channel"
                );
                Ok(Spawned { channel_id, pid })
            }
            Err(err) => {
                // The keeper refused or did not answer, so there is no PTY. The
                // binding goes with it: a recycled id must not reach the session
                // that was never opened.
                self.channels.abort_spawn(channel_id);
                tracing::warn!(channel_id, %err, "the keeper did not open the channel");
                Err(PoolError::Keeper(err))
            }
        }
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
        self.advance_past_keeper(&[channel_id]);
        tracing::info!(channel_id, pid, "adopted a surviving channel");
    }

    /// Write input without waiting for an answer.
    ///
    /// The keystroke path: a round trip per character is what made the terminal
    /// feel broken, so the unacknowledged frame is the default and
    /// [`KeeperPool::input_sequenced`] is for a caller that needs the receipt.
    pub fn input(&self, channel_id: u16, bytes: &[u8]) -> Result<(), PoolError> {
        self.require_connected()?;
        self.request(|client| client.write_input(channel_id, bytes))
    }

    /// Write input and wait for the keeper to say how much of it landed.
    pub fn input_sequenced(
        &self,
        channel_id: u16,
        input_seq: u64,
        bytes: &[u8],
    ) -> Result<PtyInResult, PoolError> {
        self.require_connected()?;
        self.request(|client| client.write_input_sequenced(channel_id, input_seq, bytes))
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

    /// The channels the keeper says it still owns, for a reconcile.
    pub fn keeper_channels(&self) -> Result<Vec<KeeperChannelBinding>, PoolError> {
        self.require_connected()?;
        self.request(|client| client.list_channels().map(|list| list.channels))
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

    /// Move the id allocator past every channel a keeper reported.
    pub fn advance_past_keeper(&self, keeper_channels: &[u16]) -> bool {
        let mut allocator = self.lock_allocator();
        let moved = allocator.advance_past_keeper(keeper_channels);
        if moved {
            tracing::info!(
                next = allocator.next(),
                keeper_channels = keeper_channels.len(),
                "advanced the channel id allocator past the keeper"
            );
        }
        moved
    }

    /// Drop a channel from the pool entirely, for a session that has closed.
    pub fn forget(&self, channel_id: u16) -> Option<PoolChannel> {
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

    /// The keeper is gone: tell every channel this worker drives, once.
    ///
    /// `on_error` and not `on_exit`, because the keeper outlives the connection
    /// and every one of those PTYs may still be running in it. Ending them here
    /// would report a terminal somebody is typing into as finished, and the
    /// reconnect would have to re-adopt channels this pool had already closed.
    pub fn keeper_lost(&self, reason: String) {
        if !self.connected.swap(false, Ordering::SeqCst) {
            // Exactly once: a second report is the same event seen twice, and a
            // second ending for one channel is the defect this guards.
            tracing::debug!(%reason, "the keeper connection was already reported gone");
            return;
        }
        let channels = self.channels.drain();
        tracing::error!(
            %reason,
            channels = channels.len(),
            "the keeper connection is gone; every channel it drove was told"
        );
        for channel in channels {
            // A channel that already ended was told so by its own exit frame;
            // telling it again here is the double ending this pool must not do.
            if !channel.has_exited() {
                channel.output().on_error(reason.clone());
            }
        }
    }

    fn take_channel_id(&self) -> Result<u16, PoolError> {
        self.lock_allocator().take().ok_or(PoolError::NoChannelId)
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
    fn request<T>(
        &self,
        use_client: impl FnOnce(&KeeperClient) -> Result<T, ClientError>,
    ) -> Result<T, PoolError> {
        let outcome = self.keeper.with(use_client);
        if let Err(ClientError::Io(reason)) = &outcome {
            self.keeper_lost(format!("the keeper connection failed: {reason}"));
        }
        Ok(outcome?)
    }

    fn require_connected(&self) -> Result<(), PoolError> {
        if self.connected.load(Ordering::SeqCst) {
            return Ok(());
        }
        Err(PoolError::Disconnected(
            "the keeper connection was reported gone".into(),
        ))
    }

    fn lock_allocator(&self) -> MutexGuard<'_, ChannelAllocator> {
        match self.allocator.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}
