//! The worker's live session set and the one way a session ends. A record is
//! CREATED by `spawn`/`resume`/`respawn` and DESTROYED only here. Depends on
//! `channel_fsm`, `strays` for the post-close tail window, and the event sink.
//!
//! THE TABLE IS HERE BECAUSE THIS IS THE ONLY PLACE A RECORD APPEARS OR
//! DISAPPEARS. `session::emit` and `browser_commands::scrollback_page` both need
//! a record by id, and a second live set would be a second answer to "does this
//! worker hold that session" — the question a browser command's reply turns on.
//! A SESSION ENDS EXACTLY ONCE, AND ONE THIS WORKER HELD IS NEVER TOMBSTONED
//! TWICE. `channel_fsm` guarantees the first: the `closed` emission rides on the
//! transition, so a caller can neither forget it nor repeat it. The second is
//! held by the recently-closed set. Killing twice must not tell the coordinator
//! one channel ended twice, while a session this worker NEVER held still needs
//! a tombstone: an orphan whose keeper died is otherwise unkillable.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use roost_observability::clock::EventClock;
use roost_protocol::wire::brand::{SessionId, WorkerFp};
use roost_protocol::wire::event::SessionEvent;

use super::binding::{CellDelivery, ChannelDelivery};
use super::keeper_channels::KeeperChannels;
use super::respawn::DeadBirths;
use super::sinks::SessionEventSink;
use super::spawn::{ShellSpawner, ShellSpecResolver};
use crate::browser_commands::Refusal;
use crate::browser_commands::session_lifecycle::SessionOutcome;
use crate::event_store::{DurableEventKind, Reservation};
use crate::strays::RECENTLY_CLOSED_TTL;

/// The live session index. Its own file because it is a second type with a
/// second question, and `SessionManager` is a different one. Re-exported so the
/// split moved no caller.
pub use super::table::SessionTable;
/// The worker's sessions, as a browser command acts on them. Data holders and
/// interfaces only: the manager owns no clock, no socket and no core, so every
/// one is injected. `Arc<SessionManager>` is the `SessionLifecycle` a dispatch
/// runs against.
pub struct SessionManager {
    // `pub(super)`, not private: the other session modules are siblings rather
    // than children, and each reads the clock, the table and the sink.
    pub(super) worker_fp: WorkerFp,
    pub(super) sessions: Arc<SessionTable>,
    pub(super) events: Arc<dyn SessionEventSink>,
    pub(super) keeper: Arc<dyn KeeperChannels>,
    pub(super) cells: Arc<Mutex<dyn CellDelivery>>,
    /// Where a channel's bytes are parsed and shipped, as opposed to what
    /// `cells` registers. Two objects, one implementation: a delivery change and
    /// a parse decision are different questions.
    pub(super) ingest: Arc<Mutex<dyn ChannelDelivery>>,
    pub(super) clock: Arc<dyn EventClock>,
    /// The PTY-opening seam and the folder→spec resolver `session::spawn`
    /// borrows, injected rather than constructed so the manager owns no socket
    /// and no filesystem.
    pub(super) spawner: Arc<dyn ShellSpawner>,
    pub(super) resolver: Arc<dyn ShellSpecResolver>,
    /// The stillborn births this worker has seen, and whether they say the
    /// keeper itself is handing out PTYs that print nothing.
    pub(super) dead_births: DeadBirths,
    /// This manager, as an owned handle, for the futures it builds.
    ///
    /// `Weak`, and set at CONSTRUCTION rather than default: `SessionLifecycle`'s
    /// methods return `Boxed<T>`, which is `Pin<Box<dyn Future + Send +
    /// 'static>>`, and a `&self` receiver cannot put itself into a `'static`
    /// future. So a method reached through `Arc<dyn SessionLifecycle>` clones
    /// this, upgrades it, and builds its future from the OWNED `Arc` — which is
    /// what makes that future `Send + 'static` while the trait keeps `&self` and
    /// stays dyn compatible. `Arc` on the receiver was the other answer and the
    /// compiler refused it: an undispatchable receiver cannot back a `dyn`.
    ///
    /// `Weak` and not `Arc` so the manager does not own itself. `new` returns
    /// `Arc<Self>` precisely so this is never absent — a handle that could be
    /// missing before it is needed is a handle that will be.
    self_handle: Weak<Self>,
    /// The highest resize sequence asked of the keeper, per channel. Seeded
    /// from what the keeper reports it applied, because this worker's own count
    /// begins at zero and the keeper rejects a sequence it has already applied.
    pub(super) resize_seqs: Mutex<HashMap<u16, u64>>,
    /// The one channel-id counter, kept beside the reaper that advances it past
    /// the keeper's maximum rather than beside the code that hands ids out.
    pub(super) channels: Mutex<crate::strays::ChannelAllocator>,
    /// Wall-clock ms at which this worker last closed each of these sessions.
    pub(super) recently_closed: Mutex<HashMap<SessionId, i64>>,
}

impl std::fmt::Debug for SessionManager {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let live = self.sessions.live().len();
        formatter
            .debug_struct("SessionManager")
            .field("worker_fp", &self.worker_fp)
            .field("live_sessions", &live)
            .finish_non_exhaustive()
    }
}

impl SessionManager {
    /// The manager every dependency is threaded through.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        worker_fp: WorkerFp,
        sessions: Arc<SessionTable>,
        events: Arc<dyn SessionEventSink>,
        keeper: Arc<dyn KeeperChannels>,
        cells: Arc<Mutex<dyn CellDelivery>>,
        ingest: Arc<Mutex<dyn ChannelDelivery>>,
        clock: Arc<dyn EventClock>,
        spawner: Arc<dyn ShellSpawner>,
        resolver: Arc<dyn ShellSpecResolver>,
    ) -> Arc<Self> {
        // `new_cyclic`, and the reason is not style. `Arc::get_mut` requires
        // the weak count to be ZERO, so filling the handle after construction
        // could never succeed — the struct's own placeholder `Weak` is already a
        // weak reference, and the `expect` below proved it at runtime rather
        // than at compile time. `new_cyclic` hands the `Weak` to the closure, so
        // the handle is correct from the first instant and there is no window in
        // which `owned()` could return `None` on a manager that exists.
        Arc::new_cyclic(|self_handle| Self {
            worker_fp,
            sessions,
            events,
            keeper,
            cells,
            ingest,
            clock,
            spawner,
            resolver,
            dead_births: DeadBirths::default(),
            channels: Mutex::new(crate::strays::ChannelAllocator::new()),
            recently_closed: Mutex::new(HashMap::new()),
            resize_seqs: Mutex::new(HashMap::new()),
            self_handle: self_handle.clone(),
        })
    }

    /// This manager as an owned handle, for a future that must outlive `&self`.
    ///
    /// `None` is a DURABILITY REFUSAL and it fails closed, deliberately. If no
    /// `Arc<SessionManager>` exists then nothing owns this manager, so a
    /// capacity claim taken through it could not be tied to anything durable —
    /// which is exactly the condition the reservation exists to refuse. A caller
    /// that cannot be given a durable sink is told so rather than answered.
    pub fn owned(&self) -> Option<Arc<Self>> {
        self.self_handle.upgrade()
    }

    /// Claim durable capacity for one event of this session's own.
    pub async fn reserve(&self, kind: DurableEventKind) -> Result<Reservation, Refusal> {
        self.events
            .reserve(kind)
            .await
            .map_err(|error| Refusal::failed("sessions", error.to_string()))
    }

    /// Give a durable claim back, because the event it was taken for will not
    /// happen.
    ///
    /// PUBLIC, and not only because a guard needs somewhere to put a claim
    /// back: a caller that has reserved and then decided not to use it holds
    /// nothing but the manager. `Reservation` is `Copy` with no `Drop`, so a
    /// claim that goes out of scope is not given back — it keeps its row and
    /// its `default_reserved_bytes` against the store's caps, and
    /// `Store::reserve` tests `rows + reserved_rows`. Enough of those and
    /// every later session spawn is refused `Full`, several restarts removed
    /// from anything the log would have said.
    pub async fn release_reservation(&self, reservation: Reservation) {
        self.events.release(reservation).await;
    }

    /// Close one channel: emit its `closed` event once, then let the record go.
    /// The record leaves the table BEFORE the emission, so a second close — the
    /// keeper's own exit after a kill, or a kill after it — finds nothing to
    /// close and emits nothing. That is the exactly-once property, and it is
    /// why this takes `&self`: two callers reach it concurrently.
    pub async fn close_channel(
        &self,
        channel_id: u16,
        exit_code: Option<i32>,
    ) -> Result<SessionOutcome, Refusal> {
        let Some(entry) = self.sessions.forget(channel_id) else {
            return Ok(SessionOutcome::Killed);
        };
        // EVERYTHING THAT NEEDS THE RECORD HAPPENS INSIDE THIS BLOCK, so the
        // guard goes out of SCOPE before the first `.await` below rather than
        // being dropped by hand. That distinction is load-bearing and it cost a
        // compile to find: an explicit `drop(record)` in one branch leaves a
        // drop flag, and with a drop flag the compiler still considers the
        // `MutexGuard` live across every await in the function — which makes
        // this future `!Send` and takes `SessionManager` with it. Scoping is the
        // only version the borrow checker can prove.
        let (session_id, branded_channel_id, trace_id, now_ms, reservation, head_seq, closes) = {
            let mut record = entry
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let session_id = record.session_id().clone();
            let branded_channel_id = record.channel_id();
            let trace_id = record.identity.session_trace_id.clone();
            let now_ms = self.clock.now_epoch_ms();
            let reservation = record.close_reservation;
            let head_seq = record.head_seq;
            super::respawn::note_birth(self, &record, now_ms);
            let transition = record
                .fsm
                .close(exit_code)
                .map_err(|refusal| Refusal::failed("sessions", refusal.reason()))?;
            (
                session_id,
                branded_channel_id,
                trace_id,
                now_ms,
                reservation,
                head_seq,
                transition.closes,
            )
        };
        let Some(closed_code) = closes else {
            // No guard to release here: the block above already ended.
            self.events.release(reservation).await;
            return Err(Refusal::failed(
                "sessions",
                format!("channel {channel_id} closed without ending: {closes:?}"),
            ));
        };
        let event = SessionEvent::Closed {
            session_id: session_id.clone(),
            exit_code: closed_code.map(i64::from),
            ts: now_ms,
            trace_id: Some(trace_id),
        };
        // No guard is in scope here either, for the same reason as above.
        let recorded = self.events.emit(&event, Some(reservation)).await;
        self.cells
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .forget_channel(branded_channel_id);
        self.mark_recently_closed(&session_id, now_ms);
        match recorded {
            Ok(()) => {
                tracing::info!(
                    session_id = %session_id,
                    channel_id,
                    exit_code = ?closed_code,
                    head_seq,
                    "a session ended and its close was recorded"
                );
                Ok(SessionOutcome::Killed)
            }
            // The coordinator will believe this session is alive forever, and
            // no retry fixes that: this worker is the only writer of the answer.
            Err(error) => {
                tracing::error!(
                    session_id = %session_id,
                    channel_id,
                    head_seq,
                    error = %error,
                    "a session ended and its close could not be recorded"
                );
                Ok(SessionOutcome::DurabilityLost)
            }
        }
    }

    /// A session closed within the post-close tail window: a channel keeps
    /// emitting after its record is gone, and the emit path asks this first.
    ///
    pub fn is_recently_closed(&self, session_id: &SessionId, now_ms: i64) -> bool {
        let window = RECENTLY_CLOSED_TTL.as_millis() as i64;
        let held = self.recently_closed.lock().ok();
        held.is_some_and(|closed| {
            closed
                .get(session_id)
                .is_some_and(|at| now_ms.saturating_sub(*at) < window)
        })
    }

    fn mark_recently_closed(&self, session_id: &SessionId, now_ms: i64) {
        let Ok(mut closed) = self.recently_closed.lock() else {
            return;
        };
        let window = RECENTLY_CLOSED_TTL.as_millis() as i64;
        closed.retain(|_, at| now_ms.saturating_sub(*at) < window);
        closed.insert(session_id.clone(), now_ms);
    }

    /// Tell a closed session's coordinator row that the session is over, for a
    /// session this worker NEVER held. One it held already emitted its `closed`;
    /// an orphan still needs one, because a row whose keeper died can never be
    /// closed from a browser.
    async fn tombstone(&self, session_id: &SessionId) -> Result<SessionOutcome, Refusal> {
        let now_ms = self.clock.now_epoch_ms();
        if self.is_recently_closed(session_id, now_ms) {
            tracing::debug!(
                session_id = %session_id,
                "a kill named a session this worker already closed; nothing is emitted"
            );
            return Ok(SessionOutcome::Killed);
        }
        let event = SessionEvent::Closed {
            session_id: session_id.clone(),
            exit_code: None,
            ts: now_ms,
            trace_id: None,
        };
        match self
            .events
            .emit(&event, Some(self.reserve(DurableEventKind::Closed).await?))
            .await
        {
            Ok(()) => {
                tracing::info!(
                    session_id = %session_id,
                    "a kill named a session this worker never held; a close tombstone was recorded"
                );
                Ok(SessionOutcome::Killed)
            }
            Err(error) => {
                tracing::error!(
                    session_id = %session_id,
                    error = %error,
                    "a close tombstone could not be recorded"
                );
                Ok(SessionOutcome::DurabilityLost)
            }
        }
    }

    /// End a session on a browser's request. A kill SUPERSEDES rather than
    /// queues: the record is closed here and now and the PTY write is not
    /// waited on, because a closed pane must not outlive its request.
    pub async fn kill_held_session(
        &self,
        session_id: &SessionId,
    ) -> Result<SessionOutcome, Refusal> {
        let Some(channel_id) = self.sessions.channel_of(session_id) else {
            return self.tombstone(session_id).await;
        };
        if let Err(fault) = self.keeper.kill_channel(channel_id) {
            // A keeper that has already lost the channel is the case the close
            // below repairs, so the fault is logged and the close still runs:
            // the session is over either way.
            tracing::warn!(
                session_id = %session_id,
                channel_id,
                error = %fault,
                "the keeper would not take a kill; the session is closed anyway"
            );
        }
        self.close_channel(channel_id, Some(0)).await
    }
}
