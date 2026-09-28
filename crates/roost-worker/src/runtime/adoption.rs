//! Adopting the PTYs a surviving keeper still holds, at boot. Called once by
//! `runtime::boot_sequence`, after the keeper pool is admitted and the session
//! layer is built over it, and by nothing else. Depends on `session::resume`,
//! `keeper_pool` and `runtime::reconcile` — and on nothing that depends on it
//! back.
//!
//! EVERY IDENTITY HERE IS THE COORDINATOR'S. The session id and the folder are
//! fields of the coordinator's own open-session row, matched on the CHANNEL the
//! keeper reports. A survivor adopted under an id this worker made up would be a
//! second session wearing the channel of the first, and nothing would notice
//! until a browser attached to the wrong terminal.
//!
//! A SURVIVOR IS NOT ADOPTED UNTIL THE KEEPER HAS PROVEN IT CAN DESCRIBE IT,
//! and that is the whole point of this file. `KeeperPool::channel_history` is
//! a HARD REFUSAL against every keeper this build speaks to
//! (`keeper_pool/session_seam.rs` names `NO_REPORTED_HEAD` and
//! `NO_REPORTED_BASE_GEOMETRY`; the protocol reports neither), so
//! `session::resume::adopt_survivor` cannot complete against a real keeper —
//! and it has already DONE THE PART OF THE ADOPTION THAT MUTATES THE POOL
//! before it gets there: it calls `deliver_into` at `resume.rs:201`, which
//! rebinds that channel's keeper output into a `RecordBinding` in `Staged`
//! mode, and only then asks for the history at `:204` and fails.
//!
//! SO THE MEASURED DAMAGE TODAY IS SILENT ORPHANING, NOT A KILL, and this file
//! states which is which rather than the worse story. The `channel_history`
//! refusal returns through a plain `map_err` and does NOT call `abandon` — the
//! measured test `a_survivor_the_keeper_cannot_describe_is_left_running_
//! rather_than_killed` still finds the child alive with the probe disabled. So
//! the PTY survives, while: its output is routed into a staging buffer that
//! parses nothing, `Staging::stage_output` DISCARDS the held events outright
//! once `RESUME_STAGE_CAP_BYTES` is passed, no record is ever installed, no
//! browser can attach, the coordinator still lists the session open, and the
//! close claim this file reserved is never released. Every live terminal on
//! the machine is unreachable, lossy and invisible, and nothing logs an
//! error.
//!
//! THE KILL IS ONE KEEPER-PROTOCOL FIX AWAY, and that is the reason the gate
//! is not optional. `abandon` — which calls `keeper.kill_channel` — runs on
//! `adopted_record` failure, on the table insert failing, and on staging
//! overflow (`resume.rs:225`, `:232`, `:251`). All three are REACHABLE THE
//! MOMENT `channel_history` starts answering, which is what W-K's missing
//! `GetHistory` and `GetTerminalState` client frames are for; and
//! `session_adoption.rs`'s
//! `a_survivor_whose_replay_does_not_converge_on_the_keepers_geometry_is_
//! refused` already drives that path today against a scripted keeper. The
//! same boot that silently orphans every terminal today would kill every
//! terminal the day the keeper learned to answer.
//!
//! THE DISTINCTION THE PROBE MAKES IS "WE CANNOT ADOPT THIS" AGAINST "THIS
//! IS BROKEN AND MUST DIE". They are different facts with different
//! responses, and the code conflated them by routing every refusal into one
//! counter. A keeper that cannot report a head is a GAP IN THE PROTOCOL; the
//! PTY behind it is a live terminal somebody is looking at. It is HELD —
//! not adopted, not abandoned, not killed, AND NOT REBOUND — and the boot
//! continues. "Not rebound" is the part that is silent today and is the whole
//! reason the probe runs before `deliver_into` rather than after it.
//!
//! THE PROBE IS A KEEPER READ AND NOTHING ELSE. It runs per channel, before
//! the adoption is attempted, through the same [`KeeperChannels`] seam the
//! adoption itself uses, so there is one answer to "can this keeper describe
//! this channel" and it is read from one place. It is a refusal the EXPECTED
//! outcome against a keeper that cannot report history, so it is logged at
//! `info` and never at `error`.
//!
//! A CHANNEL THE COORDINATOR DOES NOT LIST IS LEFT ALONE for a different
//! reason and by a different rule: there is no session identity to adopt it
//! into, and killing a session the coordinator has already closed is not this
//! worker's decision to take.
//!
//! EVERY REFUSAL IS COUNTED AND LOGGED, NEVER PROPAGATED. A refusal AFTER the
//! probe passed is `session::resume`'s designed repair — the history was
//! proven replayable, so an adoption that still cannot complete kills the
//! survivor and the session must be respawned. But propagating any of it
//! would abort the boot and take every OTHER survivor and the worker's link
//! down with it, over one terminal. A refused adoption is NOT a boot
//! failure.

use std::sync::Arc;

use roost_observability::clock::EventClock;
use roost_protocol::wire::brand::{ChannelId, SessionId, TraceId};

use super::keeper_boot::KeeperBootOutcome;
use super::reconcile::OpenSession;
use super::session_stack::SessionStack;
use crate::event_store::DurableEventKind;
use crate::keeper_pool::KeeperPool;
use crate::session::resume::{AdoptionRequest, AdoptRefusal, KeeperChannels, KeeperFault};

/// What reconciling the keeper's survivors against the session table did.
///
/// FIVE COUNTERS AND NOT ONE, because the outcomes are different facts: two of
/// them left a live PTY running and three did not, and a single "unreplayable"
/// count cannot tell an operator which happened.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Adopted {
    /// Channels the keeper still held and this worker adopted around.
    pub adopted: usize,
    /// Channels whose history could not be brought into a cold core, so NO
    /// adoption was attempted. Every one of these was left RUNNING and
    /// undisturbed — held, not killed, not adopted, not abandoned — and the
    /// boot continued. Either the channel is not one this worker can address,
    /// or the replayability probe found the keeper cannot describe it. Against
    /// any keeper that cannot report a history head — which is every keeper
    /// this build speaks to — this is the expected outcome, not a fault.
    pub unreplayable: usize,
    /// Channels the probe PASSED and whose adoption then failed. Each of these
    /// was KILLED, because this is `session::resume`'s designed repair and
    /// `abandon` runs on every one of these paths — the history was proven
    /// replayable, so a replay that still cannot complete is a stream that
    /// cannot be made whole and the respawn is the only repair that preserves
    /// the no-gap invariant. Each leaves its session to be respawned. This is
    /// the counter an operator reads when a terminal was actually ended.
    pub refused: usize,
    /// Channels the keeper holds that the coordinator does not list as open, so
    /// there is no session identity to adopt them into. These were left RUNNING
    /// and undisturbed.
    pub unknown_to_coordinator: usize,
    /// Survivors whose adoption had no durable capacity reserved for its end.
    /// Left running, because a session that cannot record its own close must not
    /// be made live here.
    pub unreservable: usize,
}

/// Whether the keeper can describe this channel's history well enough to
/// replay it into a cold core.
///
/// THE GATE, and it is a keeper READ: it asks for the history and reports
/// whether the keeper produced one. It changes nothing — no binding is
/// installed, no record is taken, no claim is reserved — so a refusal here
/// leaves the survivor exactly as the last worker left it.
///
/// The two refusals this exists to tell apart live one level apart. A
/// refusal HERE means the worker does not know how to bring this terminal
/// forward, and the survivor is left running. A refusal INSIDE
/// [`crate::session::resume::adopt_survivor`] means the worker knew how and
/// could not, and the survivor is killed. Passing the same fault through both
/// is what made a restart destructive.
///
/// `Ok` here is necessary and not sufficient: the adoption still does the
/// reattach, the geometry check and the atomic swap, and can still refuse. It
/// only establishes that the refusal will not be a refusal to DESCRIBE.
fn probe_replayable(pool: &KeeperPool, channel: u16) -> Result<(), KeeperFault> {
    KeeperChannels::channel_history(pool, channel).map(|_| ())
}

/// Adopt every channel the keeper holds that the coordinator still lists open.
///
/// `held` is the channel list the keeper admission already read, rather than a
/// second read: the pool learns the ids as a side effect of listing them, and a
/// boot that listed twice would be a second answer to "what does the keeper
/// hold" arriving after the ids were already spent.
pub async fn adopt_survivors(
    stack: &SessionStack,
    pool: &Arc<KeeperPool>,
    held: &[u16],
    open: &[OpenSession],
    socket_path: &str,
) -> Adopted {
    let mut adopted = Adopted::default();
    let trace = stack.manager.trace_id();
    for raw in held.iter().copied() {
        let Some(channel_id) = ChannelId::try_from(i64::from(raw)).ok() else {
            adopted.unreplayable += 1;
            tracing::warn!(
                channel_id = raw,
                "boot: the keeper holds a channel this worker cannot address, so it was \
                 neither adopted nor disturbed"
            );
            continue;
        };
        // THE REPLAYABILITY PROBE, IN FRONT OF THE ADOPTION, AND IT IS THE
        // ONLY WAY PAST. `adopt_survivor` kills whatever it refuses, and the
        // refusal this probe is here to intercept is the common one: against a
        // keeper that reports no history head, EVERY channel reaches it and
        // EVERY live terminal on this machine dies on every restart. So the
        // keeper is asked first, the answer decides, and a refusal leaves the
        // survivor alone.
        //
        // `info`, NOT `error`, and that level is the difference between "this
        // build cannot adopt yet" and "this machine lost its terminals". The
        // boot continues either way: one undescribable channel is not a
        // reason to take every other survivor and the link down with it.
        if let Err(fault) = probe_replayable(pool, raw) {
            adopted.unreplayable += 1;
            tracing::info!(
                channel_id = raw,
                operation = %fault.operation,
                reason = %fault.reason,
                "boot: the keeper cannot describe this survivor's history, so it was NOT \
                 adopted; it is LEFT RUNNING and undisturbed, because a worker that cannot \
                 rebuild a record around a terminal is not evidence the terminal is broken"
            );
            continue;
        }
        // Matched on the CHANNEL, not on the id: the keeper names the channel it
        // still holds and the coordinator names the session that channel is. A
        // row that does not name this channel is a row for a different session,
        // and adopting across that gap would put one session's history under
        // another's id.
        let Some(row) = open.iter().find(|row| row.channel == u32::from(raw)) else {
            adopted.unknown_to_coordinator += 1;
            tracing::warn!(
                channel_id = raw,
                "boot: the keeper holds a channel the coordinator does not list as open, so \
                 there is no session identity to adopt it into; it was left running and \
                 undisturbed rather than killed"
            );
            continue;
        };
        let Some(session_id) = SessionId::try_from(row.id.clone()).ok() else {
            adopted.unknown_to_coordinator += 1;
            tracing::warn!(
                channel_id = raw,
                session = %row.id,
                "boot: the coordinator's session id for this channel is not one this worker \
                 can address, so the survivor was left alone rather than adopted under a \
                 substituted id"
            );
            continue;
        };
        // The close claim is taken BEFORE the adoption, so a survivor that cannot
        // be recorded never becomes live here. A refusal releases it again
        // inside `adopt_survivor`, which is why holding it across the call is
        // safe rather than a leak.
        let close = match stack.manager.reserve(DurableEventKind::Closed).await {
            Ok(reservation) => reservation,
            Err(refusal) => {
                adopted.unreservable += 1;
                tracing::error!(
                    channel_id = raw,
                    session_id = %session_id,
                    reason = %refusal.message(),
                    "boot: there is no durable capacity to record this survivor's end, so it \
                     was NOT adopted and NOT killed; a session that cannot record its own end \
                     must not be made live here"
                );
                continue;
            }
        };
        // The folder is the coordinator's SPAWN folder when the row carries one
        // and its current `cwd` otherwise: a session re-opened in the folder its
        // shell drifted to is a different session wearing this id.
        //
        // `spawn_cwd` IS AN `Option` in the generated row — the proto field is
        // not a defaulted string — so the "carries one" test is `Some` and
        // non-empty, not `is_empty`. An absent field falls through to `cwd`,
        // which is the same folder the row would name anyway.
        let folder = match row.spawn_cwd.as_deref().filter(|value| !value.is_empty()) {
            Some(spawn_cwd) => spawn_cwd.to_owned(),
            None => row.cwd.clone(),
        };
        let shell_spec = match stack.resolve_shell_spec(&row.cwd, &row.id) {
            Ok(spec) => spec,
            Err(reason) => {
                adopted.unreservable += 1;
                tracing::error!(
                    channel_id = raw,
                    session_id = %session_id,
                    %reason,
                    "boot: this survivor's launch contract could not be resolved, so it was \
                     NOT adopted and NOT killed; a record whose PTY was opened under a \
                     different contract is not this session"
                );
                continue;
            }
        };
        let request = AdoptionRequest {
            session_id,
            channel_id,
            folder,
            shell_spec,
            close_reservation: close,
            session_trace_id: trace.clone().unwrap_or_else(minted_trace),
            // The coordinator's own stream generation, and it is NOT here yet:
            // the `Session` row carries no stream field, and an adopted session
            // emits no `opened`, so a generation this worker invented would be
            // addressed by nobody. Empty is the honest value and the log line
            // above says which field is missing.
            stream_id: String::new(),
            socket_path: socket_path.to_owned(),
            now_ms: stack.clock.now_epoch_ms(),
            mono_ms: stack.clock.mono_ns() / 1_000_000,
        };
        match stack.manager.adopt_survivor(&request).await {
            Ok(result) => {
                adopted.adopted += 1;
                tracing::info!(
                    channel_id = raw,
                    session_id = %request.session_id,
                    replay_offset = result.replay_offset,
                    head_seq = result.head_seq,
                    "boot: a keeper survivor was adopted and its history replayed into a \
                     cold core"
                );
            }
            // EVERY ARM BELOW THIS POINT IS A REFUSAL THE PROBE DID NOT
            // PREVENT, so every one of them killed its survivor — `abandon`
            // runs on each. They count as `refused`, not `unreplayable`,
            // because the two fields answer different questions: `refused`
            // means a terminal was ended here, and an operator reading the boot
            // line needs to be able to tell that from a survivor this worker
            // declined to touch.
            Err(AdoptRefusal::Unreplayable { channel, reason }) => {
                adopted.refused += 1;
                tracing::warn!(
                    channel_id = channel,
                    session_id = %request.session_id,
                    %reason,
                    "boot: the keeper described this survivor's history and the replay still \
                     failed, so IT was killed and its session must be respawned; every other \
                     survivor and the link are untouched"
                );
            }
            Err(AdoptRefusal::StagingOverflow { channel, cap }) => {
                adopted.refused += 1;
                tracing::warn!(
                    channel_id = channel,
                    session_id = %request.session_id,
                    cap,
                    "boot: this survivor produced more concurrent output than an adoption can \
                     stage, so IT was killed and its session must be respawned; every other \
                     survivor and the link are untouched"
                );
            }
            Err(refusal) => {
                adopted.refused += 1;
                tracing::warn!(
                    %refusal,
                    channel_id = raw,
                    session_id = %request.session_id,
                    "boot: this survivor was not adopted; the refusal names which half of the \
                     adoption it stopped at"
                );
            }
        }
    }
    adopted
}

/// The channel ids a keeper admission proved this worker must not collide with.
///
/// `Adopted` from the admission is the list the PROBE read, which is the same
/// list `keeper_channels` would return; taking it from the admission rather
/// than asking again is what keeps the id counter and the adoption loop looking
/// at one reading of the same question.
pub fn channels_from(outcome: &KeeperBootOutcome) -> Vec<u16> {
    match outcome {
        KeeperBootOutcome::Adopted { channels, .. } => channels.clone(),
        KeeperBootOutcome::StartedFresh { .. } => Vec::new(),
        KeeperBootOutcome::Held { .. } => Vec::new(),
    }
}

/// The trace an adopted survivor's record carries when the worker minted one.
///
/// [`AdoptionRequest`] wants the coordinator's own trace so every event about
/// that session correlates. The `Session` row this worker reads has no trace
/// field, so the value here is one this process minted — a VALID id, named as
/// minted rather than passed off as the coordinator's.
fn minted_trace() -> TraceId {
    crate::session::ids::mint_trace_id()
        .ok()
        .and_then(|value| TraceId::try_from(value).ok())
        .unwrap_or_else(|| {
            TraceId::try_from("0000000000000000")
                .unwrap_or_else(|_| unreachable!("16 zeroes is a trace id"))
        })
}
