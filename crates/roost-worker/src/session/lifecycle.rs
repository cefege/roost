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

use std::collections::{HashMap, Mutex, MutexGuard};
use std::sync::Arc;

use roost_observability::clock::EventClock;
use roost_protocol::wire::brand::{SessionId, WorkerFp};
use roost_protocol::wire::event::SessionEvent;

use super::binding::{CellDelivery, ChannelDelivery};
use super::respawn::DeadBirths;
use super::resume::KeeperChannels;
use super::sinks::SessionEventSink;
use super::spawn::{ShellSpawner, ShellSpecResolver};
use super::types::SessionRecord;
use crate::browser_commands::Refusal;
use crate::browser_commands::session_lifecycle::SessionOutcome;
use crate::event_store::{DurableEventKind, Reservation};
use crate::strays::RECENTLY_CLOSED_TTL;

/// The live records, by channel and by session.
#[derive(Debug, Default)]
struct LiveSet {
    by_channel: HashMap<u16, Arc<Mutex<SessionRecord>>>,
    by_session: HashMap<SessionId, u16>,
}

/// The worker's live sessions: the one answer to "does this worker hold it".
/// Every read hands the record to a closure rather than returning a guard, so no
/// caller names this type's lock.
#[derive(Debug, Default)]
pub struct SessionTable {
    live: Mutex<LiveSet>,
}

impl SessionTable {
    /// Hold a new record. An id already here is refused rather than overwritten:
    /// replacing one leaves a PTY whose bytes reach a record nobody can address.
    pub fn insert(&self, record: SessionRecord) -> Result<Arc<Mutex<SessionRecord>>, Refusal> {
        let channel_id = record.channel_id();
        let session_id = record.session_id().clone();
        let mut live = self.lock();
        if live.by_channel.contains_key(&channel_id) || live.by_session.contains_key(&session_id) {
            return Err(Refusal::failed(
                "sessions",
                format!("channel {channel_id} or session {session_id} is already live here"),
            ));
        }
        let entry = Arc::new(Mutex::new(record));
        live.by_channel.insert(channel_id, Arc::clone(&entry));
        live.by_session.insert(session_id, channel_id);
        Ok(entry)
    }

    /// Read the live record for a session id.
    pub fn with_record<R>(
        &self,
        session_id: &SessionId,
        read: impl FnOnce(&SessionRecord) -> R,
    ) -> Option<R> {
        let channel_id = self.lock().by_session.get(session_id).copied()?;
        self.with_channel_record(channel_id, read)
    }

    /// Change the live record for a session id. Separate from the read form
    /// because taking `&mut` is how a caller stops being a reader.
    pub fn with_record_mut<R>(
        &self,
        session_id: &SessionId,
        change: impl FnOnce(&mut SessionRecord) -> R,
    ) -> Option<R> {
        let channel_id = self.lock().by_session.get(session_id).copied()?;
        let entry = Arc::clone(self.lock().by_channel.get(&channel_id)?);
        let mut record = entry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Some(change(&mut record))
    }

    /// Read the live record for a keeper channel id.
    pub fn with_channel_record<R>(
        &self,
        channel_id: u16,
        read: impl FnOnce(&SessionRecord) -> R,
    ) -> Option<R> {
        let entry = Arc::clone(self.lock().by_channel.get(&channel_id)?);
        let record = entry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Some(read(&record))
    }

    /// The channel a session lives on, or `None` when this worker does not hold
    /// it.
    pub fn channel_of(&self, session_id: &SessionId) -> Option<u16> {
        self.lock().by_session.get(session_id).copied()
    }

    /// Every live session and its channel, in no particular order.
    pub fn live(&self) -> Vec<(SessionId, u16)> {
        self.lock()
            .by_session
            .iter()
            .map(|(session, channel)| (session.clone(), *channel))
            .collect()
    }

    /// The record a channel's bytes are delivered into.
    pub(super) fn entry(&self, channel_id: u16) -> Option<Arc<Mutex<SessionRecord>>> {
        self.lock().by_channel.get(&channel_id).map(Arc::clone)
    }
    /// Every live session and its channel, in no particular order.
    pub fn live(&self) -> Vec<(SessionId, u16)> {
        self.lock()
            .by_session
            .iter()
            .map(|(session, channel)| (session.clone(), *channel))
            .collect()
    }

    /// Remove a channel's record. The returned entry is the last reference, so a
    /// second close finds nothing to close.
    pub(super) fn forget(&self, channel_id: u16) -> Option<Arc<Mutex<SessionRecord>>> {
        let mut live = self.lock();
        let entry = live.by_channel.remove(&channel_id)?;
        let held: Vec<SessionId> = live
            .by_session
            .iter()
            .filter(|(_, channel)| **channel == channel_id)
            .map(|(session, _)| session.clone())
            .collect();
        for session_id in held {
            live.by_session.remove(&session_id);
        }
        Some(entry)
    }

    fn lock(&self) -> MutexGuard<'_, LiveSet> {
        self.live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

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
    ) -> Self {
        Self {
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
        }
    }

    /// Claim durable capacity for one event of this session's own.
    pub fn reserve(&self, kind: DurableEventKind) -> Result<Reservation, Refusal> {
        self.events
            .reserve(kind)
            .map_err(|error| Refusal::failed("sessions", error.to_string()))
    }

    /// Close one channel: emit its `closed` event once, then let the record go.
    /// The record leaves the table BEFORE the emission, so a second close — the
    /// keeper's own exit after a kill, or a kill after it — finds nothing to
    /// close and emits nothing. That is the exactly-once property, and it is
    /// why this takes `&self`: two callers reach it concurrently.
    pub fn close_channel(
        &self,
        channel_id: u16,
        exit_code: Option<i32>,
    ) -> Result<SessionOutcome, Refusal> {
        let Some(entry) = self.sessions.forget(channel_id) else {
            return Ok(SessionOutcome::Killed);
        };
        let mut record = entry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let session_id = record.session_id().clone();
        let trace_id = record.identity.session_trace_id.clone();
        let now_ms = self.clock.now_epoch_ms();
        let reservation = record.close_reservation;
        let head_seq = record.head_seq;
        super::respawn::note_birth(self, &record, now_ms);
        let transition = record
            .fsm
            .close(exit_code)
            .map_err(|refusal| Refusal::failed("sessions", refusal.reason()))?;
        let Some(closed_code) = transition.closes else {
            self.events.release(reservation);
            return Err(Refusal::failed(
                "sessions",
                format!("channel {channel_id} left {transition:?} without closing"),
            ));
        };
        let event = SessionEvent::Closed {
            session_id: session_id.clone(),
            exit_code: closed_code.map(i64::from),
            ts: now_ms,
            trace_id: Some(trace_id),
        };
        let recorded = self.events.emit(&event, Some(reservation));
        drop(record);
        drop(entry);
        self.cells
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .forget_channel(channel_id);
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
    fn tombstone(&self, session_id: &SessionId) -> Result<SessionOutcome, Refusal> {
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
            .emit(&event, Some(self.reserve(DurableEventKind::Closed)?))
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
    pub fn kill_held_session(&self, session_id: &SessionId) -> Result<SessionOutcome, Refusal> {
        let Some(channel_id) = self.sessions.channel_of(session_id) else {
            return self.tombstone(session_id);
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
        self.close_channel(channel_id, Some(0))
    }
}
