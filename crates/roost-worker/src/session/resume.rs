//! Rebuilding a record around a PTY whose keeper survived this worker: the
//! reattach at the history's ordered boundary, the replay into a cold core,
//! the staged concurrent output, and the atomic swap. Ports
//! `apps/worker/src/session/session-resume.ts` (`resume`). The boot and
//! keeper-death reconcile (`runtime::session_reconcile`) is the only caller; a
//! refusal is its signal to respawn the logical session.
//!
//! THE ORDER IS v2's. The core is admitted, then the survivor is reattached and
//! its history read at one keeper boundary (`KeeperChannels::reattach_with_history`),
//! then the applied geometry is read, then the core is rebuilt and the record
//! installed, and only then does the staged output go live. Every failure after
//! the reattach KILLS the survivor and records its end before any capacity is
//! released, so no orphan outlives its durable close (v2 `resume` catch).
//!
//! OVERFLOW REFUSES THE ADOPTION. A PTY stream is contiguous, so discarding
//! either end of the staged window splices a hole into parser state nothing
//! downstream re-parses; the survivor is killed and respawned instead.

use std::sync::Arc;

use roost_protocol::wire::brand::{ChannelId, SessionId};
use roost_protocol::wire::event::SessionEvent;

use super::binding::{RESUME_STAGE_CAP_BYTES, RecordBinding};
use super::lifecycle::SessionManager;
use super::sinks::ChannelBinding;
use crate::event_store::Reservation;
use crate::shell_spec::ShellSpec;
use crate::terminal_core_capacity::{
    TerminalCoreAllocationKind, TerminalCoreCapacityError, TerminalCoreLease,
};

/// What an adoption did: the window's floor, and the head it was seeded with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Adopted {
    pub replay_offset: u64,
    pub head_seq: u64,
}

/// Why a survivor was not adopted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdoptRefusal {
    #[error("the keeper holds no channel {0} to adopt")]
    NoSurvivor(u16),
    #[error("this worker already holds channel {0}")]
    AlreadyHeld(u16),
    #[error("channel {channel} could not be adopted: {reason}")]
    Unreplayable { channel: u16, reason: String },
    /// Concurrent output outgrew the staging bound while the core was rebuilt.
    #[error(
        "resume staged output exceeded {cap} bytes; adopting channel {channel} would splice a byte gap"
    )]
    StagingOverflow { channel: u16, cap: usize },
    /// Terminal-core admission refused an adoption core. Nothing was touched
    /// and the close claim was given back (v2 `session-resume.ts:294`); the
    /// reconcile stops rather than respawning.
    #[error("channel {channel} could not be adopted: {refusal}")]
    TerminalCoreCapacity {
        channel: u16,
        refusal: TerminalCoreCapacityError,
    },
}

/// What a survivor is adopted as.
#[derive(Debug, Clone)]
pub struct AdoptionRequest {
    pub session_id: SessionId,
    pub channel_id: ChannelId,
    /// The record's `cwd`: the resolved launch folder.
    pub folder: String,
    /// The launch contract a later respawn reuses verbatim.
    pub shell_spec: ShellSpec,
    /// Capacity claimed for the close that ends this session. OWNED by the
    /// adoption from the call on: held on success, spent on a tombstone or a
    /// close on failure, released when nothing was touched.
    pub close_reservation: Reservation,
}

/// A refusal, and whether the survivor was killed before it was returned.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{refusal}")]
pub struct AdoptFailure {
    pub refusal: AdoptRefusal,
    pub abandoned: bool,
}

impl AdoptFailure {
    /// A refusal that left the survivor running.
    pub fn left_alone(refusal: AdoptRefusal) -> Self {
        Self {
            refusal,
            abandoned: false,
        }
    }

    /// A refusal that killed the survivor.
    pub fn abandoned(refusal: AdoptRefusal) -> Self {
        Self {
            refusal,
            abandoned: true,
        }
    }
}

impl std::ops::Deref for AdoptFailure {
    type Target = AdoptRefusal;

    fn deref(&self) -> &Self::Target {
        &self.refusal
    }
}

/// How far an adoption got, which decides what its failure must undo.
struct Progress {
    lease: Option<TerminalCoreLease>,
    binding: Option<Arc<RecordBinding>>,
    record_installed: bool,
}

impl SessionManager {
    /// v2 `resume`: rebuild a record around a surviving PTY, or refuse and
    /// leave the logical session to a respawn.
    pub async fn adopt_survivor(&self, request: &AdoptionRequest) -> Result<Adopted, AdoptFailure> {
        let channel = request.channel_id.as_u32() as u16;
        if self.sessions.entry(channel).is_some() {
            self.events.release(request.close_reservation).await;
            return Err(AdoptFailure::left_alone(AdoptRefusal::AlreadyHeld(channel)));
        }
        let mut progress = Progress {
            lease: None,
            binding: None,
            record_installed: false,
        };
        match self.adopt_stages(request, channel, &mut progress).await {
            Ok(adopted) => Ok(adopted),
            Err(Stop::LeftAlone(refusal)) => Err(AdoptFailure::left_alone(refusal)),
            Err(Stop::Failed(reason)) => {
                self.abandon_adoption(request, channel, progress).await;
                Err(AdoptFailure::abandoned(reason))
            }
        }
    }

    async fn adopt_stages(
        &self,
        request: &AdoptionRequest,
        channel: u16,
        progress: &mut Progress,
    ) -> Result<Adopted, Stop> {
        let unreplayable =
            |reason: String| Stop::Failed(AdoptRefusal::Unreplayable { channel, reason });
        let live = self
            .keeper
            .live_channels()
            .map_err(|fault| unreplayable(fault.to_string()))?;
        let Some(survivor) = live.iter().find(|held| held.channel_id == channel) else {
            self.events.release(request.close_reservation).await;
            return Err(Stop::LeftAlone(AdoptRefusal::NoSurvivor(channel)));
        };
        progress.lease = match self
            .core_capacity
            .reserve(TerminalCoreAllocationKind::Adoption)
        {
            Ok(lease) => Some(lease),
            Err(refusal) => {
                self.events.release(request.close_reservation).await;
                return Err(Stop::LeftAlone(AdoptRefusal::TerminalCoreCapacity {
                    channel,
                    refusal,
                }));
            }
        };
        let binding = RecordBinding::closing(self, channel);
        progress.binding = Some(Arc::clone(&binding));
        let history = self
            .keeper
            .reattach_with_history(
                channel,
                survivor.pid,
                Arc::clone(&binding) as Arc<dyn ChannelBinding>,
            )
            .map_err(|fault| unreplayable(fault.to_string()))?;
        let applied = self
            .keeper
            .terminal_state(channel)
            .map_err(|fault| unreplayable(fault.to_string()))?;
        let now_ms = self.clock.now_epoch_ms();
        let record = self
            .adopted_record(request, &history, &applied, survivor.pid, now_ms)
            .map_err(Stop::Failed)?;
        let entry = self
            .sessions
            .insert(record)
            .map_err(|error| unreplayable(error.to_string()))?;
        if let Some(lease) = progress.lease.take()
            && let Err(misuse) = self.core_capacity.install_channel(channel, lease)
        {
            tracing::error!(channel_id = channel, error = %misuse, "an adopted core's lease could not become resident");
        }
        progress.record_installed = true;
        self.note_applied_resize_seq(channel, applied.applied_seq);
        self.terminal_streams
            .note_applied_size(request.channel_id, applied.cols, applied.rows);
        let (replay_offset, head_seq, stream_id) = {
            let record = entry
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (
                record.history_floor(),
                record.head_seq,
                record.cell_emit.stream_id.clone(),
            )
        };
        // The swap and its drain are ONE critical section inside the binding, so
        // a chunk the keeper delivers after the flip is parsed after the staged
        // bytes and never before them.
        let (clean, held_exit) = binding.go_live();
        if !clean {
            return Err(Stop::Failed(AdoptRefusal::StagingOverflow {
                channel,
                cap: RESUME_STAGE_CAP_BYTES,
            }));
        }
        if let Some(exit_code) = held_exit {
            // The survivor was found and its real exit was delivered: the
            // reconcile counts it handled rather than respawning it.
            tracing::info!(session_id = %request.session_id, channel_id = channel, exit_code, "a survivor exited before its record went live; the adoption closes it");
            if let Err(refusal) = self.close_channel(channel, Some(exit_code)).await {
                tracing::error!(channel_id = channel, reason = %refusal.message(), "an adopted survivor's exit could not be closed");
            }
            return Ok(Adopted {
                replay_offset,
                head_seq,
            });
        }
        self.events.hold(request.close_reservation).await;
        self.cells
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .install_stream(request.channel_id, &stream_id);
        self.notify_session_folder(&request.session_id, channel);
        tracing::info!(
            session_id = %request.session_id,
            channel_id = channel,
            head_seq,
            replay_offset,
            base_cols = history.base_cols,
            base_rows = history.base_rows,
            history_evicted = history.evicted(),
            "session.attach: a keeper survivor was adopted and its history replayed into a cold core"
        );
        Ok(Adopted {
            replay_offset,
            head_seq,
        })
    }

    /// v2 `resume` catch: kill the survivor BEFORE any capacity is released,
    /// mark its channel recently closed, and record the session's end once.
    async fn abandon_adoption(&self, request: &AdoptionRequest, channel: u16, progress: Progress) {
        if let Err(fault) = self.keeper.kill_channel(channel) {
            tracing::error!(channel_id = channel, error = %fault, "a failed adoption's survivor would not die");
        }
        let now_ms = self.clock.now_epoch_ms();
        self.keeper_health.mark_recently_closed(channel, now_ms);
        let staged = progress
            .binding
            .as_ref()
            .map_or(0, |binding| binding.abandon());
        if self.sessions.entry(channel).is_some() {
            if let Err(refusal) = self.close_channel(channel, None).await {
                tracing::error!(channel_id = channel, reason = %refusal.message(), "a failed adoption's record could not be closed");
            }
        } else if !progress.record_installed {
            self.emit_closed_tombstone(&request.session_id, Some(request.close_reservation))
                .await;
        }
        drop(progress.lease);
        tracing::warn!(
            session_id = %request.session_id,
            channel_id = channel,
            staged_bytes = staged,
            "session.resume_downgraded_respawn: the survivor was killed and its session must be respawned"
        );
    }

    /// v2 `emitClosedTombstone`: a `closed` with no exit code for a session this
    /// worker holds no record of, under the caller's claim or a fresh one.
    pub(crate) async fn emit_closed_tombstone(
        &self,
        session_id: &SessionId,
        reservation: Option<Reservation>,
    ) {
        let reservation = match reservation {
            Some(reservation) => reservation,
            None => match self
                .reserve(crate::event_store::DurableEventKind::Closed)
                .await
            {
                Ok(reservation) => reservation,
                Err(refusal) => {
                    tracing::error!(session_id = %session_id, reason = %refusal.message(), "a close tombstone could not be reserved");
                    return;
                }
            },
        };
        let event = SessionEvent::Closed {
            session_id: session_id.clone(),
            exit_code: None,
            ts: self.clock.now_epoch_ms(),
            trace_id: None,
        };
        match self.events.emit(&event, Some(reservation)).await {
            Ok(()) => {
                tracing::info!(session_id = %session_id, "a close tombstone was recorded for a session with no live record")
            }
            Err(error) => {
                tracing::error!(session_id = %session_id, %error, "a close tombstone could not be recorded")
            }
        }
    }
}

/// Where an adoption stopped: untouched, or after the reattach.
enum Stop {
    LeftAlone(AdoptRefusal),
    Failed(AdoptRefusal),
}
