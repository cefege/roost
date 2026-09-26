use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use roost_protocol::wire::brand::{ChannelId, SessionId, TraceId};
use roost_protocol::wire::event::SessionEvent;

use super::binding::{CellDelivery, RecordBinding};
use super::lifecycle::SessionManager;
use super::resume::KeeperFault;
use super::sinks::ChannelBinding;
use super::spawn::{self, ShellSpawner, ShellSpecResolver, SpawnContext, SpawnRequest};
use super::types::SessionRecord;
use crate::browser_commands::Refusal;
use crate::browser_commands::session_lifecycle::{
    Boxed, DEFAULT_COLS, DEFAULT_ROWS, SessionLifecycle, SessionOutcome,
};
use crate::channel_fsm::ChannelEvent;
use crate::event_store::DurableEventKind;
use crate::strays::{DEAD_BIRTH_LIFETIME, DEAD_BIRTH_THRESHOLD, DEGRADED_WINDOW, Stillborn};

impl SessionManager {
    /// The next channel id to ask the keeper for, past every channel it holds.
    pub fn take_channel_id(&self) -> Result<ChannelId, Refusal> {
        let raw = self
            .channels
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .map(i64::from)
            .ok_or_else(|| {
                Refusal::failed("sessions", "every channel id on this worker is spent")
            })?;
        ChannelId::try_from(raw).map_err(|error| Refusal::failed("sessions", error.to_string()))
    }

    /// Move the channel counter past what the keeper still holds, so a fresh
    /// worker cannot collide with an orphaned PTY from an earlier keeper.
    pub fn advance_past_keeper(&self) -> Result<bool, KeeperFault> {
        let live = self.keeper.live_channels()?;
        let held: Vec<u16> = live.iter().map(|channel| channel.channel_id).collect();
        let moved = self
            .channels
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .advance_past_keeper(&held);
        if moved {
            tracing::info!(
                channels = held.len(),
                "channel ids advanced past the keeper"
            );
        }
        Ok(moved)
    }

    /// Whether this keeper is handing out PTYs that print nothing. The restart
    /// decision belongs to the keeper's owner: only a path that can replace a
    /// keeper may act on this.
    pub fn keeper_is_degraded(&self) -> bool {
        self.dead_births.keeper_is_degraded()
    }

    /// A trace id for an event this worker authors, hex over the same entropy
    /// the log lines use so one `grep` correlates the event with its line.
    /// `None` only when the kernel CSPRNG cannot be read, which is the
    /// condition the worker's signing key refuses on too.
    pub fn trace_id(&self) -> Option<TraceId> {
        use std::io::Read;
        let mut source = std::fs::File::open("/dev/urandom").ok()?;
        let mut bytes = [0_u8; roost_observability::trace::TRACE_ID_BYTES];
        source.read_exact(&mut bytes).ok()?;
        TraceId::try_from(roost_observability::trace::trace_id_from_bytes(bytes)).ok()
    }
}

/// Count a birth that just ended, and say whether the keeper has now produced
/// too many of them.
pub fn note_birth(manager: &SessionManager, record: &SessionRecord, now_ms: i64) {
    if !classify_birth(record, now_ms).is_stillborn() {
        return;
    }
    tracing::warn!(
        session_id = %record.session_id(),
        channel_id = record.channel_id().as_u32(),
        lifetime_ms = now_ms.saturating_sub(record.identity.spawned_at_ms),
        "a keeper produced a pty that exited having printed nothing"
    );
    if manager.dead_births.note(now_ms, true) {
        tracing::error!(
            dead_births = DEAD_BIRTH_THRESHOLD,
            window_ms = DEGRADED_WINDOW.as_millis() as i64,
            "the keeper is handing out pty children that print nothing"
        );
    }
}

impl SessionManager {
    /// Claim an existing session for a viewer, or say this worker has none.
    ///
    /// The offset it answers with is where the browser resumes PULLING history,
    /// not what this worker replays: a viewer that is behind gets its own
    /// offset, one that fell below the floor is told the floor, and neither is
    /// served a window this worker cannot address.
    pub fn claim_viewer(
        &self,
        session_id: &SessionId,
        from_offset: Option<u64>,
    ) -> Result<SessionOutcome, Refusal> {
        let now_ms = self.clock.now_epoch_ms();
        let claimed = self.sessions.with_record_mut(session_id, |record| {
            let attached = record.fsm.send(ChannelEvent::Attach).is_ok();
            let floor = record.history_floor();
            (
                attached,
                from_offset.unwrap_or(0).max(floor).min(record.head_seq),
            )
        });
        let Some((attached, replay_offset)) = claimed else {
            return Err(Refusal::failed(
                "attach",
                format!("this worker holds no session {session_id}"),
            ));
        };
        if attached {
            // Metadata rather than a claim: a second viewer joining an
            // already-attached channel changes nothing about the CHANNEL's
            // lifecycle, and presence is the coordinator's projection rather
            // than this event's business.
            let event = SessionEvent::Attached {
                session_id: session_id.clone(),
                ts: now_ms,
                trace_id: self.trace_id(),
            };
            self.events
                .emit(&event, None)
                .map_err(|error| Refusal::failed("attach", error.to_string()))?;
        }
        tracing::info!(
            session_id = %session_id,
            replay_offset,
            already_attached = !attached,
            "a viewer claimed a live session"
        );
        Ok(SessionOutcome::Attached { replay_offset })
    }

    /// Replace a lost child, or report that there was nothing to replace.
    ///
    /// A session this worker still holds is re-opened at the folder its PTY was
    /// opened in. One it does NOT hold has no launch contract of its own, so the
    /// caller's folder is all there is to open — which is why the reconcile path
    /// adopts survivors BEFORE this runs: respawning a session whose keeper
    /// survived throws away a live terminal's history.
    pub fn respawn_lost_child(
        &self,
        session_id: &SessionId,
        folder: &str,
        cols: u16,
        rows: u16,
    ) -> Result<SessionOutcome, Refusal> {
        self.open_under(session_id, folder, true, cols, rows)
    }

    /// Spawn a brand new shell, under a caller-minted id when one was named.
    pub fn open_shell(
        &self,
        folder: String,
        cols: Option<u16>,
        rows: Option<u16>,
        requested_session_id: Option<SessionId>,
    ) -> Result<SessionOutcome, Refusal> {
        let cols = cols.unwrap_or(DEFAULT_COLS);
        let rows = rows.unwrap_or(DEFAULT_ROWS);
        let pending = requested_session_id
            .as_ref()
            .filter(|id| !self.sessions.with_record(id, |_| ()).is_none());
        if pending.is_some() {
            return Ok(SessionOutcome::AlreadyLive);
        }
        self.open_under(requested_session_id.as_ref(), &folder, false, cols, rows)
    }

    /// The one path that opens a channel, for a spawn and for a respawn alike.
    ///
    /// `replacement` is what selects the launch contract: a session this worker
    /// still holds is re-opened at the spec its PTY was launched with, VERBATIM
    /// and un-resolved, because re-resolving a folder that has since been
    /// deleted would fail a session that was working a moment ago.
    fn open_under(
        &self,
        session_id: Option<&SessionId>,
        folder: &str,
        replacement: bool,
        cols: u16,
        rows: u16,
    ) -> Result<SessionOutcome, Refusal> {
        let held = session_id
            .and_then(|id| {
                self.sessions.with_record(id, |record| {
                    (record.channel_id(), record.identity.shell_spec.clone())
                })
            })
            .filter(|_| replacement);
        let (folder, shell_spec) = match held {
            // The launch folder, NOT the drifted `cwd`: see the header.
            Some((_, spec)) => (spec.cwd.clone(), Some(spec)),
            None => (folder.to_string(), None),
        };
        let channel_id = self.take_channel_id()?;
        let binding = RecordBinding::staged(
            channel_id.as_u32() as u16,
            Arc::clone(&self.sessions),
            Arc::clone(&self.ingest),
            Arc::clone(&self.clock),
        );
        let event = if replacement {
            DurableEventKind::State
        } else {
            DurableEventKind::Opened
        };
        let opened = self.reserve(event)?;
        let close = self.reserve(DurableEventKind::Closed)?;
        let request = SpawnRequest {
            channel_id,
            folder,
            cols,
            rows,
            session_id: session_id.cloned(),
            shell_spec,
            event,
        };
        let context = SpawnContext {
            spawner: self.spawner.as_ref(),
            resolver: self.resolver.as_ref(),
            events: self.events.as_ref(),
            worker_fp: &self.worker_fp,
        };
        let record = spawn::spawn_shell(
            &context,
            opened,
            close,
            Arc::clone(&binding) as Arc<dyn ChannelBinding>,
            request,
            self.clock.now_epoch_ms(),
        )
        .map_err(|error| {
            // The spawn path gives both claims back and kills any PTY it opened,
            // so all that is left here is to say so and answer the caller.
            tracing::warn!(
                channel_id = channel_id.as_u32(),
                session_id = ?session_id,
                error = %error,
                "a session's pty was not opened; every claim it took was given back"
            );
            Refusal::failed("sessions", error.to_string())
        })?;
        self.sessions
            .insert(record)
            .map_err(|error| Refusal::failed("sessions", error.to_string()))?;
        let stream_id = session_id
            .as_ref()
            .and_then(|id| {
                self.sessions
                    .with_record(id, |record| record.cell_emit.stream_id.clone())
            })
            .unwrap_or_default();
        binding.go_live();
        self.cells
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .install_stream(channel_id, &stream_id);
        tracing::info!(
            session_id = ?session_id,
            channel_id = channel_id.as_u32(),
            cols,
            rows,
            replacement,
            "a session's pty was opened"
        );
        Ok(SessionOutcome::Spawned {
            channel_id: channel_id.as_u32() as u16,
        })
    }
}

/// The four browser commands, as this manager answers them.
///
/// All four here, in one place, because they are one contract: a browser can
/// kill a session, open one, replace a lost one, or claim one, and what each
/// answers turns on whether this worker already holds the session. The work each
/// arm does lives in the file that owns it.
impl SessionLifecycle for SessionManager {
    fn kill(&self, session_id: SessionId) -> Boxed<Result<SessionOutcome, Refusal>> {
        let answered = self.kill_held_session(&session_id);
        Box::pin(std::future::ready(answered))
    }

    fn spawn_shell(
        &self,
        folder: String,
        cols: Option<u16>,
        rows: Option<u16>,
        requested_session_id: Option<SessionId>,
    ) -> Boxed<Result<SessionOutcome, Refusal>> {
        let answered = self.open_shell(folder, cols, rows, requested_session_id);
        Box::pin(std::future::ready(answered))
    }

    fn respawn_if_missing(
        &self,
        session_id: SessionId,
        cwd: String,
        cols: u16,
        rows: u16,
    ) -> Boxed<Result<SessionOutcome, Refusal>> {
        let answered = self.respawn_lost_child(&session_id, &cwd, cols, rows);
        Box::pin(std::future::ready(answered))
    }

    fn attach(
        &self,
        session_id: SessionId,
        from_offset: Option<u64>,
    ) -> Boxed<Result<SessionOutcome, Refusal>> {
        let answered = self.claim_viewer(&session_id, from_offset);
        Box::pin(std::future::ready(answered))
    }
}

/// The dead-birth verdict for a record that just ended.
///
/// A pure function of the record's own numbers, so the rule is testable with no
/// clock and no keeper in the way. ZERO BYTES IS THE DISCRIMINATOR, not the
/// lifetime: a real shell prints a prompt before it exits, so a fast `exit` is
/// not a dead birth.
pub fn classify_birth(record: &SessionRecord, now_ms: i64) -> Stillborn {
    let lifetime_ms = now_ms.saturating_sub(record.identity.spawned_at_ms);
    if lifetime_ms >= DEAD_BIRTH_LIFETIME.as_millis() as i64 {
        return Stillborn::LivedLongEnough;
    }
    if record.produced_output() {
        Stillborn::ProducedOutput
    } else {
        Stillborn::Stillborn
    }
}

/// The stillborn births this worker has seen inside the degraded window.
#[derive(Debug, Default)]
pub struct DeadBirths {
    inside: Mutex<VecDeque<i64>>,
    degraded: Mutex<bool>,
}

impl DeadBirths {
    /// Count one birth, and say whether the keeper has now produced too many.
    pub fn note(&self, now_ms: i64, stillborn: bool) -> bool {
        if !stillborn {
            return false;
        }
        let mut inside = self
            .inside
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let window = DEGRADED_WINDOW.as_millis() as i64;
        inside.push_back(now_ms);
        while inside
            .front()
            .is_some_and(|at| now_ms.saturating_sub(*at) >= window)
        {
            inside.pop_front();
        }
        if (inside.len() as u32) < DEAD_BIRTH_THRESHOLD {
            return false;
        }
        // Cleared rather than left to re-fire: the keeper is about to be
        // replaced, and a restart burst must not trip the threshold again while
        // that replacement is still coming up.
        inside.clear();
        *self
            .degraded
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
        true
    }

    /// Whether this keeper is handing out PTYs that print nothing.
    pub fn keeper_is_degraded(&self) -> bool {
        *self
            .degraded
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
