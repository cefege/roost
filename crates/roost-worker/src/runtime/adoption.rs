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
//! A CHANNEL THE COORDINATOR DOES NOT LIST IS LEFT ALONE, and that is the
//! decision this file exists to get right. `adopt_survivor` kills the survivor
//! it refuses, so calling it for a channel with no row to adopt into would end
//! a terminal over the worker's inability to name it. There is no identity to
//! adopt such a channel into, and killing a session the coordinator has already
//! closed is not this worker's decision to take.
//!
//! EVERY REFUSAL IS COUNTED AND LOGGED, NEVER PROPAGATED. A refusal of the
//! unreplayable kind kills the survivor it was asked about, which is
//! `session::resume`'s designed repair and not this file's to second-guess — but
//! propagating it would abort the boot and take every OTHER survivor and the
//! worker's link down with it, over one terminal whose history could not be
//! described.

use std::sync::Arc;

use roost_protocol::wire::brand::{ChannelId, SessionId, TraceId};

use super::keeper_boot::KeeperBootOutcome;
use super::reconcile::OpenSession;
use super::session_stack::SessionStack;
use crate::event_store::DurableEventKind;
use crate::keeper_pool::KeeperPool;
use crate::session::resume::{AdoptionRequest, AdoptRefusal};

/// What reconciling the keeper's survivors against the session table did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Adopted {
    /// Channels the keeper still held and this worker adopted around.
    pub adopted: usize,
    /// Channels whose history could not be replayed. Each of these was killed,
    /// because that is the repair `AdoptRefusal` documents, and each leaves its
    /// session to be respawned.
    pub unreplayable: usize,
    /// Channels the keeper holds that the coordinator does not list as open, so
    /// there is no session identity to adopt them into. These were left RUNNING
    /// and undisturbed.
    pub unknown_to_coordinator: usize,
    /// Survivors whose adoption had no durable capacity reserved for its end.
    /// Left running, because a session that cannot record its own close must not
    /// be made live here.
    pub unreservable: usize,
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
        let folder = if row.spawn_cwd.is_empty() {
            row.cwd.clone()
        } else {
            row.spawn_cwd.clone()
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
            Err(AdoptRefusal::Unreplayable { channel, reason })
            | Err(AdoptRefusal::StagingOverflow { channel, .. }) => {
                adopted.unreplayable += 1;
                tracing::warn!(
                    channel_id = channel,
                    session_id = %request.session_id,
                    %reason,
                    "boot: this survivor's history could not be replayed, so IT was killed and \
                     its session must be respawned; every other survivor and the link are \
                     untouched"
                );
            }
            Err(refusal) => {
                adopted.unreplayable += 1;
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
