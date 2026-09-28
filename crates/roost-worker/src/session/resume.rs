//! Rebuilding a record around a PTY this worker did not spawn: the keeper seam
//! the session layer drives, the adoption this file sequences, and the
//! refusal an over-long concurrent stream earns. `keeper_pool` implements
//! the seam; boot reconcile calls [`SessionManager::adopt_survivor`]. Depends on
//! `roost_keeper` for the history vocabulary and `roost_term` for the core.
//!
//! THE ORDERING IS THE WHOLE SLICE: the two READS, then the reattach, then
//! the ordered history replay into a cold core, then the concurrent output
//! staged, then the atomic swap.
//!
//! THE READS COME FIRST AND THE REATTACH COMES THIRD, and this used to say the
//! opposite. It claimed the reattach is what makes the keeper establish its
//! ordered boundary, so the history had to follow it. `deliver_into` does not
//! establish anything on the keeper's side — the keeper streams `PtyOut` to
//! every channel it holds regardless of who is reading, and
//! `keeper_pool/session_seam.rs` says so in as many words. All the reattach
//! does is insert into the pool's acknowledged table, which is a MUTATION, and
//! a mutation that ran before the two reads meant a failing `channel_history`
//! or `terminal_state` left the survivor bound to a staging buffer whose
//! record was never installed. The ordered boundary is the history request's,
//! not the reattach's, and the reads are now what precedes it.
//!
//! OVERFLOW REFUSES THE ADOPTION. A PTY stream is contiguous, so discarding
//! either end of the staged window splices an invisible hole into parser state
//! nothing downstream re-parses: a TUI's cursor-addressed partial repaint never
//! revisits a cell it believes it already painted. The only repair that preserves
//! the no-gap invariant is the respawn path, so a survivor whose output outgrows
//! the staging bound is killed and re-created rather than adopted with a hole.
//!
//! THE HEAD IS THE KEEPER'S, NEVER RE-DERIVED. [`SurvivorHistory`] carries the
//! head the keeper reports beside its records, because the sum of the retained
//! bytes is a different number the moment anything was evicted, and a head that
//! understates the stream re-aliases every absolute address a browser holds.

use std::sync::Arc;

use roost_keeper::frames::ChannelBinding as KeeperChannel;
use roost_keeper::history::HistoryRecord;
use roost_keeper::payloads::TerminalState;
use roost_protocol::wire::brand::{ChannelId, SessionId, TraceId};

use super::binding::{RESUME_STAGE_CAP_BYTES, RecordBinding};
use super::lifecycle::SessionManager;
use super::sinks::ChannelBinding;
use crate::event_store::Reservation;
use crate::shell_spec::ShellSpec;

/// Why a keeper operation could not be performed. Its own type rather than the
/// keeper client's, because the obligation differs by operation: a failed
/// channel list adopts nothing, a failed history must become a respawn.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the keeper refused `{operation}`: {reason}")]
pub struct KeeperFault {
    pub operation: &'static str,
    pub reason: String,
}

/// The keeper operations the session layer performs, and nothing else — its
/// whole view of a keeper, and a trait because `keeper_pool` owns the socket: a
/// session with its own connection would be a second owner of a machine's PTYs.
pub trait KeeperChannels: Send + Sync {
    /// The channels this keeper still holds, and each one's child pid.
    fn live_channels(&self) -> Result<Vec<KeeperChannel>, KeeperFault>;
    /// One channel's ordered history: bytes, geometry markers and the head.
    fn channel_history(&self, channel_id: u16) -> Result<SurvivorHistory, KeeperFault>;
    /// The geometry the keeper has actually applied to this channel.
    fn terminal_state(&self, channel_id: u16) -> Result<TerminalState, KeeperFault>;
    /// Deliver this channel's output into `binding` from now on. The reattach is
    /// what establishes the keeper's ordered boundary, so it MUST precede the
    /// history request.
    fn deliver_into(
        &self,
        channel_id: u16,
        binding: Arc<dyn ChannelBinding>,
    ) -> Result<(), KeeperFault>;
    /// Terminate this channel's child.
    fn kill_channel(&self, channel_id: u16) -> Result<(), KeeperFault>;
    /// Resize this channel, returning once the keeper has ACKNOWLEDGED `seq`.
    fn resize_channel(
        &self,
        channel_id: u16,
        seq: u64,
        cols: u16,
        rows: u16,
    ) -> Result<(), KeeperFault>;
}

/// One channel's ordered history: output and geometry records oldest first,
/// the head the keeper has emitted to, and the geometry the oldest retained
/// record was produced at.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SurvivorHistory {
    pub records: Vec<HistoryRecord>,
    /// The highest sequence the keeper has emitted, retained or not. Taken from
    /// the keeper rather than summed from `records`: eviction makes the sum
    /// smaller than the stream, and a head that understates it re-aliases every
    /// absolute history address a browser already holds.
    pub head_seq: u64,
    /// The geometry the oldest retained record was produced at. Required, not
    /// optional: the keeper evicts geometry records first, so the first retained
    /// record can be output whose parser context is gone, and replaying it at
    /// the wrong width paints a screen that was never on that terminal.
    pub base_cols: u16,
    pub base_rows: u16,
}

impl SurvivorHistory {
    /// The retained output bytes, oldest first and contiguous.
    pub fn window(&self) -> Vec<u8> {
        let mut window = Vec::new();
        for record in &self.records {
            if let HistoryRecord::Output { bytes, .. } = record {
                window.extend_from_slice(bytes);
            }
        }
        window
    }

    /// Whether the keeper has evicted bytes this worker can no longer see. Under
    /// eviction the first replayed byte can be the tail of a sequence whose
    /// introducer was overwritten, and a cold core would print that remnant as
    /// literal text and stick.
    pub fn evicted(&self) -> bool {
        self.head_seq > self.window().len() as u64
    }
}

/// What an adoption did: the window's floor, and the head it was seeded with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Adopted {
    pub replay_offset: u64,
    pub head_seq: u64,
}

/// Why a survivor was not adopted. Every variant has left nothing half-adopted
/// behind by the time it returns: the survivor is killed and the durable claim
/// released, so the caller's only move left is a respawn.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdoptRefusal {
    #[error("the keeper holds no channel {0} to adopt")]
    NoSurvivor(u16),
    #[error("this worker already holds channel {0} or the session it names")]
    AlreadyHeld(u16),
    #[error("the keeper's history for channel {channel} is not replayable: {reason}")]
    Unreplayable { channel: u16, reason: String },
    /// Concurrent output outgrew the staging bound. The channel is killed: a
    /// truncated adoption is a hole in the parser, not a smaller window.
    #[error(
        "channel {channel} produced more than {cap} bytes while its core was rebuilt, so \
         adopting it would splice a byte gap; it was killed instead"
    )]
    StagingOverflow { channel: u16, cap: usize },
}

/// What a survivor is adopted as.
#[derive(Debug, Clone)]
pub struct AdoptionRequest {
    pub session_id: SessionId,
    pub channel_id: ChannelId,
    /// Where the shell is: the record's `cwd` and its events'.
    pub folder: String,
    /// The launch contract a later respawn must reuse VERBATIM, resolved by the
    /// caller and deliberately not the drifted `cwd`: a PTY re-opened under a
    /// folder the shell walked into is a different session wearing this id.
    pub shell_spec: ShellSpec,
    /// Capacity claimed for the close that ends this session, taken before the
    /// survivor was adopted so a session that cannot record its end never
    /// becomes live here.
    pub close_reservation: Reservation,
    /// The trace id the session has carried since it was created, from the
    /// coordinator's row: a new one would break the correlation every event
    /// about this session has had.
    pub session_trace_id: TraceId,
    /// The stream generation the coordinator addresses this session by, NOT
    /// re-minted: an adopted session emits no `opened`, so a generation nobody
    /// was told about would be addressed by nobody.
    pub stream_id: String,
    /// The keeper socket the survivor is on, retained for a diagnostic.
    pub socket_path: String,
    pub now_ms: i64,
    /// When this adoption happened, monotonically: the pin's age is never a wall
    /// clock, so a clock step cannot forge how long ago the floor moved.
    pub mono_ms: u64,
}


/// A refusal, and the fact of whether the survivor died.
///
/// WHY THE FACT IS CARRIED AND NOT DERIVED FROM THE VARIANT. `adopt_survivor`
/// has TEN refusal exits. Three call `abandon` and therefore
/// `keeper.kill_channel`; seven return bare. Five of those seven produce
/// `AdoptRefusal::Unreplayable` — the SAME variant the abandoned record-build
/// and the abandoned table-insert produce — so a caller that infers "this
/// killed the survivor" from the variant it received is wrong five times out
/// of ten, on a path whose whole consequence is a terminal ending.
///
/// `Deref` is there so a caller that only wants the reason reads it without
/// unwrapping, and so a test naming a refusal keeps naming it.
///
/// **THERE IS DELIBERATELY NO `From<AdoptRefusal>`.** A convenience conversion
/// would let a caller wrap a refusal without saying what happened to the
/// survivor — the wrong answer being unconstructible rather than documented.
/// If a later change reaches for that `From`, the change is the defect and the
/// inconvenience is the guard.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{refusal}")]
pub struct AdoptFailure {
    /// Which half of the adoption it stopped at.
    pub refusal: AdoptRefusal,
    /// Whether the survivor was KILLED before this was returned.
    ///
    /// Set at the same place the abandonment runs, not inferred from anything
    /// the caller can see afterwards.
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

    /// A refusal that killed the survivor, because `abandon` had already run.
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
impl SessionManager {
    /// Rebuild a record around a PTY this worker did not spawn. The staging
    /// overflow is checked AFTER the swap and the record removed again.
    pub async fn adopt_survivor(&self, request: &AdoptionRequest) -> Result<Adopted, AdoptFailure> {
        let channel = request.channel_id.as_u32() as u16;
        if self.sessions.entry(channel).is_some() {
            return Err(AdoptFailure::left_alone(AdoptRefusal::AlreadyHeld(channel)));
        }
        // THIS ONE CLOSURE IS FIVE OF THE SEVEN BARE EXITS, and it is why
        // the change is this small: `left_alone` here is what makes
        // `live_channels`, `deliver_into`, `channel_history`,
        // `terminal_state` and `close_channel` say they did not kill anything,
        // even though all five return the same `AdoptRefusal::Unreplayable`
        // variant as the two that did.
        let unreplayable = |reason: String| {
            AdoptFailure::left_alone(AdoptRefusal::Unreplayable { channel, reason })
        };
        let live = self
            .keeper
            .live_channels()
            .map_err(|fault| unreplayable(fault.to_string()))?;
        let Some(survivor) = live.iter().find(|held| held.channel_id == channel) else {
            return Err(AdoptFailure::left_alone(AdoptRefusal::NoSurvivor(channel)));
        };
        let binding = RecordBinding::staged(
            channel,
            Arc::clone(&self.sessions),
            Arc::clone(&self.ingest),
            Arc::clone(&self.clock),
        );
        // THE REBIND COMES AFTER BOTH READS, AND IT IS THE ONLY MUTATION THIS
        // FUNCTION MAKES.
        //
        // `deliver_into` calls `KeeperPool::channels::adopt`, which INSERTS
        // into the pool's acknowledged table: from that moment the keeper
        // routes this channel's `PtyOut` frames into `binding` and nowhere
        // else. It is a rebind, not a read, and it was happening SECOND — the
        // history at `:204` and the geometry at `:208` could both fail with
        // the channel already rebound, leaving the pool's table holding an
        // entry, the terminal's bytes staged into a buffer that never goes
        // live, and nothing logged above `debug`.
        //
        // `terminal_state` is a real round trip — `KeeperPool::applied_geometry`
        // calls `client.terminal_state(id)` — so it fails on a keeper that is
        // up but not answering, which is exactly the case the runtime gate
        // does not cover: the gate checks `channel_history` and not this.
        //
        // Both reads now precede the rebind, so no exit from this function
        // before `adopt_survivor`'s own record-building can leave a survivor
        // bound to a binding whose record was never installed.
        let history = self
            .keeper
            .channel_history(channel)
            .map_err(|fault| unreplayable(fault.to_string()))?;
        let applied = self
            .keeper
            .terminal_state(channel)
            .map_err(|fault| unreplayable(fault.to_string()))?;
        self.keeper
            .deliver_into(channel, Arc::clone(&binding) as Arc<dyn ChannelBinding>)
            .map_err(|fault| unreplayable(fault.to_string()))?;
        // THE ABANDONMENT IS AWAITED, AND THAT IS THE WHOLE FIX. `abandon`
        // became `async` when the sink did, and this function used to call it
        // inside `inspect_err` and `map_err` closures — so the future was built
        // and immediately DROPPED, and the abandonment never ran. Nothing said
        // so: `EventFuture` is a `Pin<Box<dyn Future>>` and is not
        // `#[must_use]`, so a caller that forgets to await it compiles, links,
        // and silently does nothing. It is the same class as the `drop()` and
        // the `get_mut` findings — a construct that type-checks and does not do
        // what the reader believes — and it was found by a BASELINE rather than
        // by review, which is the only reason it was found at all.
        let record = match self.adopted_record(request, &history, &applied, survivor.pid) {
            Ok(record) => record,
            Err(refusal) => {
                self.abandon(request, &binding, channel).await;
                return Err(AdoptFailure::abandoned(refusal));
            }
        };
        let entry = match self.sessions.insert(record) {
            Ok(entry) => entry,
            Err(error) => {
                self.abandon(request, &binding, channel).await;
                return Err(AdoptFailure::abandoned(AdoptRefusal::Unreplayable {
                    channel,
                    reason: error.to_string(),
                }));
            }
        };
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
        // a chunk the keeper delivers the instant after the flip is parsed after
        // the staged bytes and never before them.
        let (clean, held_exit) = binding.go_live();
        if !clean {
            self.abandon(request, &binding, channel).await;
            return Err(AdoptFailure::abandoned(AdoptRefusal::StagingOverflow {
                channel,
                cap: RESUME_STAGE_CAP_BYTES,
            }));
        }
        // A SURVIVOR THAT HAD ALREADY EXITED is closed HERE, by the adoption,
        // and not by the binding. This is v2's split exactly: the live `onExit`
        // goes to `closedByKeeper` (`session-emit.ts:316-322`) and the adoption
        // path does not (`session-resume-events.ts:47-50`) — it replays the exit
        // and the close happens on the other side, after the record is
        // installed. It has to be here: the record now exists, and the binding's
        // live path runs on the keeper's dispatch thread where blocking is legal
        // and this runs on a runtime worker where it would panic. One question,
        // two callers, and only this one can answer it.
        if let Some(exit_code) = held_exit {
            tracing::info!(
                session_id = %request.session_id,
                %channel,
                %exit_code,
                "a survivor had already exited before its record was installed;                  the adoption closes it"
            );
            self.close_channel(channel, Some(exit_code))
                .await
                .map_err(|refusal| unreplayable(refusal.message()))?;
            // NO `install_stream` HERE, and the omission is the point: the close
            // above took the record out of the table, so installing a delivery
            // generation onto it would announce a stream for a session that has
            // already ended — the same "looks live and produces nothing" state
            // the overflow branch above refuses.
            return Ok(Adopted {
                replay_offset,
                head_seq,
            });
        }
        self.cells
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .install_stream(request.channel_id, &stream_id);
        tracing::info!(
            session_id = %request.session_id,
            channel_id = channel,
            head_seq,
            replay_offset,
            base_cols = history.base_cols,
            base_rows = history.base_rows,
            history_evicted = history.evicted(),
            "a keeper survivor was adopted and its history replayed into a cold core"
        );
        Ok(Adopted {
            replay_offset,
            head_seq,
        })
    }

    /// Leave nothing half-adopted: the survivor dies, the record goes, the
    /// claim is given back, the staged bytes are dropped.
    async fn abandon(&self, request: &AdoptionRequest, binding: &RecordBinding, channel: u16) {
        if let Err(fault) = self.keeper.kill_channel(channel) {
            tracing::error!(
                session_id = %request.session_id,
                channel_id = channel,
                error = %fault,
                "an adoption failed and the survivor it named would not die"
            );
        }
        self.sessions.forget(channel);
        self.events.release(request.close_reservation).await;
        let staged = binding.abandon();
        tracing::warn!(
            session_id = %request.session_id,
            channel_id = channel,
            staged_bytes = staged,
            "an adoption failed; the survivor was killed and the session must be respawned"
        );
    }
}
