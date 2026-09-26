//! Rebuilding a record around a PTY this worker did not spawn: the keeper seam
//! the session layer drives, the ordered replay that rebuilds a COLD core, and
//! the refusal an over-long concurrent stream earns. `keeper_pool` implements
//! the seam; boot reconcile calls [`SessionManager::adopt_survivor`]. Depends on
//! `roost_keeper` for the history vocabulary and `roost_term` for the core.
//!
//! THE ORDERING IS THE WHOLE SLICE: reattach, then ordered history replay into
//! a cold core, then concurrent output staged, then the atomic swap. The
//! reattach comes FIRST because it is what makes the keeper establish its
//! ordered boundary; the history is everything up to that boundary and the
//! staging buffer is everything after it. Reversing the first two splices bytes
//! a client already saw into a core that never parsed them.
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
use roost_term::AlacrittyCore;
use roost_term::TerminalCore;

use super::binding::{CellDelivery, RESUME_STAGE_CAP_BYTES, RecordBinding};
use super::ids::mint_uuid;
use super::lifecycle::SessionManager;
use super::resize::pin_for_adoption;
use super::ring::ScrollbackRing;
use super::sinks::ChannelBinding;
use super::stream_scan;
use super::types::{SessionIdentity, SessionRecord};
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

impl SessionManager {
    /// Rebuild a record around a PTY this worker did not spawn. The staging
    /// overflow is checked AFTER the swap and the record removed again.
    pub fn adopt_survivor(&self, request: &AdoptionRequest) -> Result<Adopted, AdoptRefusal> {
        let channel = request.channel_id.as_u32() as u16;
        if self.sessions.entry(channel).is_some() {
            return Err(AdoptRefusal::AlreadyHeld(channel));
        }
        let unreplayable = |reason: String| AdoptRefusal::Unreplayable { channel, reason };
        let live = self
            .keeper
            .live_channels()
            .map_err(|fault| unreplayable(fault.to_string()))?;
        let Some(survivor) = live.iter().find(|held| held.channel_id == channel) else {
            return Err(AdoptRefusal::NoSurvivor(channel));
        };
        let binding = RecordBinding::staged(
            channel,
            Arc::clone(&self.sessions),
            Arc::clone(&self.ingest),
            Arc::clone(&self.clock),
        );
        self.keeper
            .deliver_into(channel, Arc::clone(&binding) as Arc<dyn ChannelBinding>)
            .map_err(|fault| unreplayable(fault.to_string()))?;
        let history = self
            .keeper
            .channel_history(channel)
            .map_err(|fault| unreplayable(fault.to_string()))?;
        let applied = self
            .keeper
            .terminal_state(channel)
            .map_err(|fault| unreplayable(fault.to_string()))?;
        let record = self
            .adopted_record(request, &history, &applied, survivor.pid)
            .map_err(|refusal| {
                self.abandon(request, &binding, channel);
                refusal
            })?;
        let entry = self.sessions.insert(record).map_err(|error| {
            self.abandon(request, &binding, channel);
            unreplayable(error.to_string())
        })?;
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
        let clean = binding.go_live();
        if !clean {
            self.abandon(request, &binding, channel);
            return Err(AdoptRefusal::StagingOverflow {
                channel,
                cap: RESUME_STAGE_CAP_BYTES,
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

    /// The record a survivor becomes: a COLD core at the history's base
    /// geometry, replayed in order, its window seeded at the keeper's head.
    fn adopted_record(
        &self,
        request: &AdoptionRequest,
        history: &SurvivorHistory,
        applied: &TerminalState,
        child_pid: u32,
    ) -> Result<SessionRecord, AdoptRefusal> {
        let channel = request.channel_id.as_u32() as u16;
        let mut core = AlacrittyCore::new(history.base_cols, history.base_rows);
        replay_ordered(&mut core, history, channel)?;
        if core.cols() != applied.cols || core.rows() != applied.rows {
            return Err(AdoptRefusal::Unreplayable {
                channel,
                reason: format!(
                    "the replay ended at {}x{} and the keeper reports {}x{}",
                    core.cols(),
                    core.rows(),
                    applied.cols,
                    applied.rows
                ),
            });
        }
        let window = history.window();
        let alt = stream_scan::scan_alt_mode(&window, false);
        // Primed, not inferred: the coordinator's snapshot reads the CORE's alt
        // state and an empty core answers false, so a live alt redraw would land
        // on the main screen. A SIGWINCH is not the repair: a TUI repaints alt
        // without re-sending `?1049h`.
        if alt && !core.using_alt_screen() {
            core.write(stream_scan::ALT_ENTER_SEQUENCES[0]);
        }
        let mut record = SessionRecord::new(
            SessionIdentity {
                session_id: request.session_id.clone(),
                channel_id: request.channel_id,
                socket_path: request.socket_path.clone(),
                cwd: request.folder.clone(),
                shell_spec: request.shell_spec.clone(),
                session_trace_id: request.session_trace_id.clone(),
                spawned_at_ms: request.now_ms,
            },
            request.close_reservation,
            Box::new(core),
            // A FRESH grid epoch: the core is new, and a client must not merge
            // the grid it holds into one that never parsed those bytes. The
            // STREAM generation is the coordinator's, so frames are addressed
            // where it expects them.
            roost_term::CellEmitState::new(
                mint_uuid().unwrap_or_else(|_| UNCORRELATED_GRID_EPOCH.to_string()),
                request.stream_id.clone(),
            ),
            ScrollbackRing::default(),
        );
        // Both numbers together, through the one writer of the floor: these
        // bytes were produced by a process that is not this record's, and floor
        // and head must agree on the very first frame — which is when a client's
        // absolute row indexes are established.
        record.adopt_retained_history(&window, history.head_seq);
        record.alt_mode = alt;
        record.child_pid = Some(child_pid);
        record.sb_origin_pin = Some(pin_for_adoption(
            request.mono_ms,
            applied.cols,
            applied.rows,
            history.evicted(),
            record.terminal_core.discarded_line_count().unwrap_or(0),
            record.terminal_core.scrollback_count() as u64,
        ));
        Ok(record)
    }

    /// Leave nothing half-adopted: the survivor dies, the record goes, the
    /// claim is given back, the staged bytes are dropped.
    fn abandon(&self, request: &AdoptionRequest, binding: &RecordBinding, channel: u16) {
        if let Err(fault) = self.keeper.kill_channel(channel) {
            tracing::error!(
                session_id = %request.session_id,
                channel_id = channel,
                error = %fault,
                "an adoption failed and the survivor it named would not die"
            );
        }
        self.sessions.forget(channel);
        self.events.release(request.close_reservation);
        let staged = binding.abandon();
        tracing::warn!(
            session_id = %request.session_id,
            channel_id = channel,
            staged_bytes = staged,
            "an adoption failed; the survivor was killed and the session must be respawned"
        );
    }
}

/// Replay a survivor's records into a COLD core, in order. The geometry markers
/// are why this is not [`super::scrollback::replay_retained_into`]: a resize
/// reflows the lines above it, so a flat replay with only the final geometry
/// paints a screen the user is not looking at.
fn replay_ordered(
    core: &mut AlacrittyCore,
    history: &SurvivorHistory,
    channel: u16,
) -> Result<(), AdoptRefusal> {
    for record in &history.records {
        match record {
            HistoryRecord::Output { bytes, .. } => core.write(bytes),
            HistoryRecord::Resize { cols, rows, .. } => {
                core.resize(*cols, *rows);
                if core.cols() != *cols || core.rows() != *rows {
                    return Err(AdoptRefusal::Unreplayable {
                        channel,
                        reason: format!(
                            "the core kept {}x{} through a {}x{} marker",
                            core.cols(),
                            core.rows(),
                            cols,
                            rows
                        ),
                    });
                }
            }
        }
    }
    Ok(())
}

/// The grid epoch a record gets when the entropy source cannot be read: a valid
/// uuid, and visibly the uncorrelated case rather than a plausible id nobody
/// can search for.
const UNCORRELATED_GRID_EPOCH: &str = "00000000-0000-4000-8000-000000000000";
