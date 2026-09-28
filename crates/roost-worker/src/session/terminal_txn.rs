//! One coordinator stream state applied to a live core exactly once, under its
//! admission ticket, with a truthful outcome: when the core's validity changes
//! mid-transaction the answer is AMBIGUOUS rather than guessed. Called from
//! `session::terminal_control` through the per-channel control lane; the
//! keeper resize is `session::resize`, the history repairs `session::core_reprove`.
//! Ports `apps/worker/src/session/session-terminal-txn.ts`.
//!
//! THE KEEPER IS SYNCHRONOUS HERE, so every keeper step runs on a blocking
//! thread and the admission ticket is released once the keeper has answered
//! rather than once the request is written: the pool serialises its connection
//! across the round trip, so an input queued behind the ticket would wait for
//! the same answer either way.

use std::sync::Arc;

use roost_keeper::client_resize::ResizeRejectReason;
use roost_protocol::wire::brand::ChannelId;
use roost_protocol::wire::coord_worker::{
    TerminalStreamFailureKind as Failure, TerminalWritePhase,
};

use super::core_reprove::ReproofFor;
use super::keeper_admission::AdmissionTicket;
use super::lifecycle::SessionManager;
use super::resize::{ResizeOutcome, lock};
use super::terminal_state::{FailedStatus, StreamFacts, StreamRequestBudget, WorkerStreamResult};

/// The generation a transaction settles: what v2 closed over as `state`.
#[derive(Debug, Clone)]
pub(super) struct TxnGeneration {
    pub stream_id: String,
    pub enabled: bool,
    pub cols: u32,
    pub rows: u32,
    pub version: u64,
}

impl TxnGeneration {
    fn facts(&self) -> StreamFacts<'_> {
        StreamFacts {
            stream_id: &self.stream_id,
            enabled: self.enabled,
            cols: self.cols,
            rows: self.rows,
        }
    }
}

/// v2 `applyTerminalStreamNow`. The control lane serialises calls; a newer
/// generation supersedes this one's cell work at once, while a keeper mutation
/// already written is still reconciled and reported.
pub(super) async fn apply_terminal_stream_now(
    manager: Arc<SessionManager>,
    channel_id: ChannelId,
    generation: TxnGeneration,
    budget: Option<Arc<dyn StreamRequestBudget>>,
    ticket: AdmissionTicket,
) -> WorkerStreamResult {
    ticket.granted().await;
    let raw = channel_id.as_u32() as u16;
    let entry_seq = manager.channel_resize_seq(raw);
    let fail = |seq, failure, reason: &str, status, phase| {
        WorkerStreamResult::failed(generation.facts(), seq, failure, reason, status, phase)
    };
    let refuse = |reason: &str| {
        fail(
            entry_seq,
            Failure::RetryablePreWrite,
            reason,
            FailedStatus::Rejected,
            None,
        )
    };
    let stale = |budget: &Option<Arc<dyn StreamRequestBudget>>| {
        budget
            .as_ref()
            .is_some_and(|budget| !budget.is_current_connection() || budget.expired())
    };
    if manager.sessions.entry(raw).is_none() {
        return fail(
            entry_seq,
            Failure::SessionNotLive,
            "session is not live",
            FailedStatus::Rejected,
            None,
        );
    }
    if !manager
        .terminal_streams
        .is_current(channel_id, generation.version)
    {
        return refuse("terminal stream was superseded before keeper admission");
    }
    if budget
        .as_ref()
        .is_some_and(|budget| !budget.is_current_connection())
    {
        return refuse("worker connection was superseded before keeper admission");
    }
    if budget.as_ref().is_some_and(|budget| budget.expired()) {
        return refuse("terminal stream budget expired before keeper admission");
    }
    if !generation.enabled {
        return WorkerStreamResult::committed(generation.facts(), entry_seq, false);
    }
    if !core_valid(&manager, channel_id) {
        let reproving = Arc::clone(&manager);
        let (stream_id, version) = (generation.stream_id.clone(), generation.version);
        let reproof = tokio::task::spawn_blocking(move || {
            reproving.reprove_terminal_core(
                channel_id,
                ReproofFor {
                    version,
                    stream_id: &stream_id,
                },
            )
        })
        .await;
        let refusal = match reproof {
            Ok(Ok(())) => None,
            Ok(Err(refusal)) => Some((refusal.reason, refusal.retryable)),
            Err(join) => Some((format!("the re-proof did not finish: {join}"), false)),
        };
        if let Some((reason, retryable)) = refusal {
            let failure = if retryable {
                Failure::RetryablePreWrite
            } else {
                Failure::CoreFailed
            };
            let reason = format!("terminal core re-proof failed: {reason}");
            return fail(entry_seq, failure, &reason, FailedStatus::Rejected, None);
        }
        // The ticket is still held: the resize below follows at once, and the
        // ticket is what stops a second transaction racing the swap just made.
        if manager.sessions.entry(raw).is_none()
            || !manager
                .terminal_streams
                .is_current(channel_id, generation.version)
        {
            return refuse("terminal stream was superseded during core re-proof");
        }
        if stale(&budget) {
            return refuse("terminal stream budget expired during core re-proof");
        }
    }
    let target = (generation.cols as u16, generation.rows as u16);
    if current_geometry(&manager, raw) == Some(target) {
        ticket.release();
        return commit_with_baseline(&manager, channel_id, &generation, entry_seq, false);
    }
    if manager.sessions.entry(raw).is_none() {
        let reason = "session closed before the keeper resize";
        return fail(
            entry_seq,
            Failure::SessionNotLive,
            reason,
            FailedStatus::Rejected,
            None,
        );
    }
    if stale(&budget) {
        return refuse("terminal stream expired before the keeper resize");
    }
    let resizing = Arc::clone(&manager);
    let resized = tokio::task::spawn_blocking(move || {
        resizing.resize_channel(channel_id, target.0, target.1)
    })
    .await;
    ticket.release();
    let seq = entry_seq + 1;
    let ambiguous =
        |failure, reason: &str| fail(seq, failure, reason, FailedStatus::Ambiguous, None);
    let outcome = match resized {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(_)) => {
            let reason = "session closed before the keeper resize";
            return fail(
                entry_seq,
                Failure::SessionNotLive,
                reason,
                FailedStatus::Rejected,
                None,
            );
        }
        Err(join) => {
            let reason = format!("the keeper resize did not finish: {join}");
            return fail(
                entry_seq,
                Failure::AmbiguousBoundary,
                &reason,
                FailedStatus::Ambiguous,
                None,
            );
        }
    };
    match outcome {
        ResizeOutcome::Applied { .. } => {}
        ResizeOutcome::Unchanged => {
            return commit_with_baseline(&manager, channel_id, &generation, entry_seq, false);
        }
        ResizeOutcome::NotWritten { reason } => {
            return refuse(&format!("keeper did not admit terminal resize: {reason}"));
        }
        ResizeOutcome::Refused { reason } => {
            let failure = match reason {
                ResizeRejectReason::ChannelMissing | ResizeRejectReason::ChannelExited => {
                    Failure::SessionNotLive
                }
                ResizeRejectReason::InvalidRequest => Failure::InvalidRequest,
                _ => Failure::RetryablePreWrite,
            };
            let reason = format!("keeper rejected terminal resize: {}", reject_name(reason));
            return fail(
                seq,
                failure,
                &reason,
                FailedStatus::Rejected,
                Some(TerminalWritePhase::Written),
            );
        }
        // A capture that already trapped has nothing ambiguous left: the core is
        // latched and re-provable, which is the verdict the view owner repairs.
        ResizeOutcome::Trapped { reason } => return ambiguous(Failure::CoreFailed, &reason),
        ResizeOutcome::SessionClosed => {
            return ambiguous(
                Failure::AmbiguousBoundary,
                "session closed during resize recovery",
            );
        }
        ResizeOutcome::Unknown { boundary } => {
            let recovering = Arc::clone(&manager);
            let recovered = tokio::task::spawn_blocking(move || {
                recovering.recover_lost_ack(channel_id, &boundary)
            })
            .await;
            match recovered {
                Ok(Ok(())) => {}
                // Every recovery that fails closed TRAPS the core, and a trapped
                // core is repairable; only one that left it valid is ambiguous.
                Ok(Err(unrecovered)) => {
                    let failure = if unrecovered.trapped {
                        Failure::CoreFailed
                    } else {
                        Failure::AmbiguousBoundary
                    };
                    return ambiguous(failure, &unrecovered.reason);
                }
                Err(join) => {
                    return ambiguous(
                        Failure::AmbiguousBoundary,
                        &format!("resize recovery did not finish: {join}"),
                    );
                }
            }
        }
    }
    if !core_valid(&manager, channel_id) {
        return ambiguous(Failure::CoreFailed, "terminal core resize failed");
    }
    manager
        .terminal_streams
        .note_applied_size(channel_id, target.0, target.1);
    commit_with_baseline(&manager, channel_id, &generation, seq, true)
}

/// Install the baseline this generation owes and commit, unless the full could
/// not be encoded — then the core is latched and the answer is ambiguous.
fn commit_with_baseline(
    manager: &SessionManager,
    channel_id: ChannelId,
    generation: &TxnGeneration,
    seq: u64,
    resized: bool,
) -> WorkerStreamResult {
    if manager
        .terminal_streams
        .is_current(channel_id, generation.version)
        && install_baseline(manager, channel_id) == Some(false)
    {
        let reason = "full baseline could not be encoded";
        return WorkerStreamResult::failed(
            generation.facts(),
            seq,
            Failure::CoreFailed,
            reason,
            FailedStatus::Ambiguous,
            None,
        );
    }
    tracing::info!(%channel_id, stream_id = %generation.stream_id, seq, resized, "a terminal stream state committed");
    WorkerStreamResult::committed(generation.facts(), seq, resized)
}

/// v2 `installTerminalBaseline` under record → delivery lock order; `None`
/// when the session or its emitter is gone.
pub(super) fn install_baseline(manager: &SessionManager, channel_id: ChannelId) -> Option<bool> {
    let entry = manager.sessions.entry(channel_id.as_u32() as u16)?;
    let mut record = lock(&entry);
    let delivery = lock(&manager.ingest);
    let now_ms = manager.clock.now_epoch_ms();
    Some(
        delivery
            .stream_emission()?
            .install_baseline(&mut record, now_ms),
    )
}

pub(super) fn core_valid(manager: &SessionManager, channel_id: ChannelId) -> bool {
    lock(&manager.ingest)
        .stream_emission()
        .is_some_and(|emission| emission.core_valid(channel_id))
}

fn current_geometry(manager: &SessionManager, raw: u16) -> Option<(u16, u16)> {
    let entry = manager.sessions.entry(raw)?;
    let record = lock(&entry);
    Some((record.terminal_core.cols(), record.terminal_core.rows()))
}

/// The keeper's wire names (`protocol-terminal.ts:50-60`), which v2's reason
/// strings carry verbatim.
fn reject_name(reason: ResizeRejectReason) -> &'static str {
    match reason {
        ResizeRejectReason::ChannelMissing => "channel_missing",
        ResizeRejectReason::ChannelExited => "channel_exited",
        ResizeRejectReason::TerminalMissing => "terminal_missing",
        ResizeRejectReason::ResizeError => "resize_error",
        ResizeRejectReason::StaleSequence => "stale_sequence",
        ResizeRejectReason::UnknownSequence => "unknown_sequence",
        ResizeRejectReason::InvalidRequest => "invalid_request",
        ResizeRejectReason::Unsupported => "unsupported",
        ResizeRejectReason::Disconnected => "disconnected",
    }
}
