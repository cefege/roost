//! Ports v2 `apps/worker/src/session/session-respawn.ts` and
//! `session-respawn-admission.ts`.
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use roost_protocol::wire::brand::{ChannelId, SessionId, TraceId};
use roost_protocol::wire::event::SessionEvent;

use super::binding::RecordBinding;
use super::ids::mint_trace_id;
use super::keeper_channels::KeeperFault;
use super::lifecycle::SessionManager;
use super::sinks::ChannelBinding;
use super::spawn::{self, ClaimsOnFailure, SpawnContext, SpawnRequest};
use super::types::SessionRecord;
use crate::browser_commands::Refusal;
use crate::browser_commands::session_lifecycle::{DEFAULT_COLS, DEFAULT_ROWS, SessionOutcome};
use crate::channel_fsm::ChannelEvent;
use crate::event_store::DurableEventKind;
use crate::strays::{DEAD_BIRTH_LIFETIME, DEAD_BIRTH_THRESHOLD, DEGRADED_WINDOW, Stillborn};
use crate::terminal_core_capacity::TerminalCoreAllocationKind;

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
        self.keeper_health.dead_births.keeper_is_degraded()
    }

    /// A trace id for an event this worker authors, hex over the same entropy
    /// the log lines use so one `grep` correlates the event with its line.
    /// `None` only when the kernel CSPRNG cannot be read, which is the
    /// condition the worker's signing key refuses on too.
    pub fn trace_id(&self) -> Option<TraceId> {
        TraceId::try_from(mint_trace_id().ok()?).ok()
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
    if manager.keeper_health.dead_births.note(now_ms, true) {
        tracing::error!(
            dead_births = DEAD_BIRTH_THRESHOLD,
            window_ms = DEGRADED_WINDOW.as_millis() as i64,
            "keeper.degraded: the keeper is handing out pty children that print nothing"
        );
        manager.keeper_health.degraded();
    }
}

impl SessionManager {
    /// Claim an existing session for a viewer, or say this worker has none.
    ///
    /// The offset it answers with is where the browser resumes PULLING history,
    /// not what this worker replays: a viewer that is behind gets its own
    /// offset, one that fell below the floor is told the floor, and neither is
    /// served a window this worker cannot address.
    pub async fn claim_viewer(
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
                .await
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

    /// Spawn a brand new shell, under a caller-minted id when one was named.
    pub async fn open_shell(
        &self,
        folder: String,
        cols: Option<u16>,
        rows: Option<u16>,
        requested_session_id: Option<SessionId>,
    ) -> Result<SessionOutcome, Refusal> {
        let _lease = self.admit_channel_creation()?;
        let cols = cols.unwrap_or(DEFAULT_COLS);
        let rows = rows.unwrap_or(DEFAULT_ROWS);
        let pending = requested_session_id
            .as_ref()
            .filter(|id| !self.sessions.with_record(id, |_| ()).is_none());
        if pending.is_some() {
            return Ok(SessionOutcome::AlreadyLive);
        }
        self.open_under(requested_session_id.as_ref(), &folder, cols, rows)
            .await
    }

    /// The one path that opens a NEW session's channel; a respawn of a held or
    /// listed session is `respawn_replace`.
    async fn open_under(
        &self,
        session_id: Option<&SessionId>,
        folder: &str,
        cols: u16,
        rows: u16,
    ) -> Result<SessionOutcome, Refusal> {
        let folder = folder.to_string();
        let channel_id = self.take_channel_id()?;
        let binding = RecordBinding::closing(self, channel_id.as_u32() as u16);
        let event = DurableEventKind::Opened;
        let opened = self.reserve(event).await?;
        let close = match self.reserve(DurableEventKind::Closed).await {
            Ok(close) => close,
            Err(refusal) => {
                self.events.release(opened).await;
                return Err(refusal);
            }
        };
        let request = SpawnRequest {
            channel_id,
            folder,
            cols,
            rows,
            session_id: session_id.cloned(),
            shell_spec: None,
            event,
            core_allocation: TerminalCoreAllocationKind::Fresh,
            claims: ClaimsOnFailure::Release,
        };
        let context = SpawnContext {
            spawner: self.spawner.as_ref(),
            resolver: self.resolver.as_ref(),
            events: self.events.as_ref(),
            worker_fp: &self.worker_fp,
            core_capacity: &self.core_capacity,
        };
        let record = spawn::spawn_shell(
            &context,
            opened,
            close,
            Arc::clone(&binding) as Arc<dyn ChannelBinding>,
            request,
            self.clock.now_epoch_ms(),
        )
        .await
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
        let raw_channel = channel_id.as_u32() as u16;
        let opened_session = record.session_id().clone();
        let stream_id = record.cell_emit.stream_id.clone();
        if let Err(error) = self.sessions.insert(record) {
            // No record holds the core any more, so neither does its lease.
            self.core_capacity.release_channel(raw_channel);
            return Err(Refusal::failed("sessions", error.to_string()));
        }
        let (_, held_exit) = binding.go_live();
        self.cells
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .install_stream(channel_id, &stream_id);
        tracing::info!(
            session_id = ?session_id,
            channel_id = channel_id.as_u32(),
            cols,
            rows,
            "a session's pty was opened"
        );
        self.notify_session_folder(&opened_session, raw_channel);
        if let Some(exit_code) = held_exit
            && let Err(refusal) = self.close_channel(raw_channel, Some(exit_code)).await
        {
            tracing::error!(channel_id = raw_channel, reason = %refusal.message(), "a channel that exited before going live could not be closed");
        }
        Ok(SessionOutcome::Spawned {
            channel_id: channel_id.as_u32() as u16,
        })
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
