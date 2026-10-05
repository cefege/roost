//! The foreground liveness watchdog, fired by the sweep.
//!
//! Split out of `handle_sweep` because the other deadlines here are "a request
//! is due"; this one is "a pane that stopped painting owes an answer". It reads
//! the replica's liveness state, and every branch ends in one of the three
//! outcomes that leave the pane watched: a published challenge, a re-armed
//! probe, or retirement because nothing is viewed.
//!
//! There is no timer in this crate, so this function is the ONLY place either
//! deadline can come due. A host that sweeps on an interval runs the watchdog; a
//! host that forgets to cannot silently lose it, because nothing else can fire.
//!
//! Depends on `repair` for the scoped resync it publishes and on
//! `terminal::liveness` for the state it reads and writes; called only by
//! `handle_sweep`.

use super::repair::scoped_resync;
use crate::effect::Effect;
use crate::store::Store;
use crate::terminal::TerminalSession;
use crate::terminal::TerminalToken;
use crate::terminal::liveness::{RepairOutcome, StallAction};
use crate::terminal::token::TerminalTransport;

/// Why the close reason is `"terminal-liveness"` and not a network failure: a
/// pane that stopped painting is not a flaky network, and the redial must not
/// back off as though it were.
const LIVENESS_REASON: &str = "terminal-liveness";

/// Whether this replica is still something the watchdog owes anything.
enum Watch {
    /// The elected generation, still the publication target, with a view the
    /// foreground can see.
    Watched(TerminalToken),
    /// Nothing is owed any more, and this is why.
    Retire(RepairOutcome),
}

/// One pass over one replica's liveness deadlines.
pub fn sweep_foreground_liveness(
    store: &mut Store,
    session_id: &str,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    // A hidden document watches nothing. Output silence while nobody is looking
    // is not a stall, and arming a probe there would demand a repair for a pane
    // the operator cannot see.
    if !store.sync.redial.visible() {
        retire(store, session_id, RepairOutcome::Inactive);
        return;
    }
    let (proof_due, quiet_due) = match store.terminal(session_id) {
        Some(replica) => (
            replica.liveness().proof_expiry_due(now_ms),
            replica.liveness().quiet_expiry_due(now_ms),
        ),
        None => return,
    };
    // The proof first: it is the escalation, and the one whose outcome the
    // operator is waiting for. A challenge that has not timed out yet has its
    // quiet probe answered by the frame it is waiting on, so nothing is lost by
    // reading it second.
    if proof_due {
        escalate(store, session_id, now_ms, out);
    }
    if quiet_due {
        probe(store, session_id, now_ms, out);
    }
    arm_when_watchable(store, session_id, now_ms);
}

/// The quiet probe came due: publish a challenge, or re-arm and report.
fn probe(store: &mut Store, session_id: &str, now_ms: u64, out: &mut Vec<Effect>) {
    let owner = match watch(store, session_id) {
        Watch::Retire(outcome) => {
            retire(store, session_id, outcome);
            return;
        }
        Watch::Watched(owner) => owner,
    };
    let anchor = store
        .terminal_mut_if_present(session_id)
        .map(TerminalSession::liveness_mut)
        .and_then(|liveness| liveness.take_quiet());
    if publish_challenge(store, session_id, &owner, now_ms, out) {
        return;
    }
    // Nothing went out, so the session owes a DEADLINE rather than a challenge.
    // It re-arms from `now_ms` and never from the frame timestamp the previous
    // attempt read: that timestamp is already older than the interval, so
    // re-arming on it is the spin `docs/FAILURE-INDEX.md` records under this
    // feature — a watchdog that deletes itself on the one failure it exists for.
    let age_ms = anchor.map(|anchor| now_ms.saturating_sub(anchor));
    let report = store
        .terminal_mut_if_present(session_id)
        .is_some_and(|replica| replica.liveness_mut().claim_rearm_episode());
    let Some(replica) = store.terminal_mut_if_present(session_id) else {
        return;
    };
    if report {
        signal_stall(replica, StallAction::Rearm, age_ms);
    }
    replica.liveness_mut().rearm_quiet(&owner, now_ms);
}

/// The proof deadline came due: hand the generation to the recovery that
/// already replaces a Sync socket, rather than inventing a second one.
fn escalate(store: &mut Store, session_id: &str, now_ms: u64, out: &mut Vec<Effect>) {
    let owner = match watch(store, session_id) {
        Watch::Retire(outcome) => {
            retire(store, session_id, outcome);
            return;
        }
        Watch::Watched(owner) => owner,
    };
    let Some(replica) = store.terminal_mut_if_present(session_id) else {
        return;
    };
    if !replica.liveness().has_pending_challenge(&owner) {
        // A due deadline with no challenge behind it is owed nothing; discharge
        // it rather than re-arming a watchdog that has nothing to prove.
        replica.liveness_mut().clear_proof();
        return;
    }
    let age_ms = replica
        .liveness()
        .challenged_at_ms()
        .map_or(0, |challenged| now_ms.saturating_sub(challenged));
    signal_stall(replica, StallAction::Redial, Some(age_ms));
    replica.liveness_mut().mark_escalated();
    // A direct carrier is closed by the host that owns it, and this crate has no
    // effect for that; the carrier lane's own liveness is what notices a link
    // that stopped carrying. So the challenge goes back out one interval later
    // rather than the session being left holding no deadline at all.
    if owner.transport != TerminalTransport::Sync {
        rearm(store, session_id, &owner, now_ms);
        return;
    }
    let replaceable = store
        .sync
        .link_generation()
        .is_some_and(|generation| store.sync.accepts(generation));
    if !replaceable {
        // There is no live socket to replace, so the recovery would be a no-op.
        // Re-arm rather than report an escalation that never happened.
        rearm(store, session_id, &owner, now_ms);
        return;
    }
    crate::handle_sync::request_link_replacement(store, LIVENESS_REASON, out);
}

/// Publish the scoped resync a challenge is, and record what it owes an answer.
/// False when there is nothing to publish, or a challenge is already
/// outstanding — the pending challenge is the sole coalescer, so concurrent
/// deadlines cannot multiply one gap into a storm.
fn publish_challenge(
    store: &mut Store,
    session_id: &str,
    owner: &TerminalToken,
    now_ms: u64,
    out: &mut Vec<Effect>,
) -> bool {
    let Some(replica) = store.terminal(session_id) else {
        return false;
    };
    if replica.liveness().has_pending_challenge(owner) {
        return false;
    }
    let Some(request) = scoped_resync(replica, owner, session_id) else {
        return false;
    };
    out.push(request);
    let Some(replica) = store.terminal_mut_if_present(session_id) else {
        return false;
    };
    replica.begin_scoped_repair(owner, now_ms);
    // A pane that keeps answering with its proof alone is idle, not stuck: only
    // the first challenge after output is a stall worth the always-on channel.
    let streak = replica.liveness().proved_idle_streak();
    if streak == 0 {
        signal_stall(replica, StallAction::Resync, None);
    } else {
        tracing::debug!(target: "terminal", session_id = %replica.session_id, streak,
            "an idle pane re-proves its lane");
    }
    true
}

/// Arm the probe for a replica that is watched and has nothing armed.
///
/// "Watched" is an EXPECTED STREAM and a view the foreground can see — not a
/// baseline. The pane that is silently broken is exactly the one whose baseline
/// never arrived: a dropped baseline leaves the replica expecting a stream with
/// nothing behind it, and every rule that fires on a REFUSAL stays silent
/// because nothing was refused. v2 covers that shape from the view LEASE
/// (`armTerminalViewRenewal` → `requestTerminalLivenessChallenge`), which is a
/// second timer with a different owner; this is the one place a sweep already
/// runs, so the arm belongs here.
///
/// Never EXTENDS an armed deadline: this runs on every sweep, and an extension
/// would push the probe out one sweep at a time until it could never come due.
fn arm_when_watchable(store: &mut Store, session_id: &str, now_ms: u64) {
    let Some(owner) = elected_owner(store, session_id) else {
        return;
    };
    let Some(replica) = store.terminal_mut_if_present(session_id) else {
        return;
    };
    if replica.expected_stream_id().is_none() || replica.foreground_view().is_none() {
        return;
    }
    // While a challenge is outstanding the proof deadline owns the session, and
    // a probe armed behind it would report an unpublishable-challenge episode
    // for a challenge that was published.
    if replica.liveness().has_pending_challenge(&owner) {
        return;
    }
    replica.liveness_mut().arm_quiet_if_unarmed(&owner, now_ms);
}

/// The generation whose publication target this session currently has, or `None`
/// when it has none.
///
/// The generation fence belongs HERE as much as at fire time: a pane mid-
/// rotation is bound to a socket that is already gone, and arming a probe under
/// it would re-arm every sweep the retirement the fence just earned — a probe
/// that fires, retires, re-arms, fires, for as long as the rotation lasts.
fn elected_owner(store: &Store, session_id: &str) -> Option<TerminalToken> {
    let owner = store.terminal(session_id)?.generation()?.clone();
    let target = super::publication_target(store, session_id);
    (target.as_ref() == Some(&owner)).then_some(owner)
}

/// Re-validate every precondition at fire time, because a deadline outlives the
/// state that armed it.
fn watch(store: &Store, session_id: &str) -> Watch {
    let Some(replica) = store.terminal(session_id) else {
        return Watch::Retire(RepairOutcome::Inactive);
    };
    if replica.foreground_view().is_none() {
        return Watch::Retire(RepairOutcome::Inactive);
    }
    if let Some(challenged) = replica.liveness().challenge_stream_id()
        && replica.expected_stream_id() != Some(challenged)
    {
        return Watch::Retire(RepairOutcome::StreamReplaced);
    }
    match elected_owner(store, session_id) {
        Some(owner) => Watch::Watched(owner),
        None => Watch::Retire(unwatchable_reason(store, session_id)),
    }
}

/// Why there is no elected owner: a replica bound to a generation is bound to a
/// carrier that has moved on, and one with no generation never had a route this
/// document could watch.
fn unwatchable_reason(store: &Store, session_id: &str) -> RepairOutcome {
    if store
        .terminal(session_id)
        .and_then(TerminalSession::generation)
        .is_some()
    {
        RepairOutcome::GenerationReset
    } else {
        RepairOutcome::Inactive
    }
}

/// Retire a replica's deadlines, and only when it holds some: a sweep must not
/// stamp `repair_outcome` on a replica that was never watched.
fn retire(store: &mut Store, session_id: &str, outcome: RepairOutcome) {
    let Some(replica) = store.terminal_mut_if_present(session_id) else {
        return;
    };
    if replica.liveness().quiet_due_ms().is_none()
        && replica.liveness().challenged_at_ms().is_none()
    {
        return;
    }
    replica.retire_liveness(outcome);
    tracing::debug!(
        target: "terminal",
        session_id,
        outcome = outcome.as_str(),
        "foreground liveness retired"
    );
}

fn rearm(store: &mut Store, session_id: &str, owner: &TerminalToken, now_ms: u64) {
    if let Some(replica) = store.terminal_mut_if_present(session_id) {
        replica.liveness_mut().rearm_quiet(owner, now_ms);
    }
}

/// One payload shape for every transition this layer reports, so a field added
/// for one of them reaches the other two. `roost doctor` groups on
/// `layer`/`action`, and the action is what tells a re-armed probe apart from a
/// challenge that went out.
fn signal_stall(replica: &TerminalSession, action: StallAction, age_ms: Option<u64>) {
    let liveness = replica.liveness();
    let owner = replica
        .generation()
        .or_else(|| liveness.owner())
        .or_else(|| liveness.challenge_generation());
    let (checkpoint_stream_id, checkpoint_seq) =
        replica.canonical().map_or((None, None), |frame| {
            (Some(frame.stream_id.as_str()), Some(frame.seq))
        });
    tracing::warn!(
        target: "terminal",
        session_id = %replica.session_id,
        layer = "terminal_proof",
        action = action.as_str(),
        age_ms,
        expected_stream_id = replica.expected_stream_id().unwrap_or(""),
        checkpoint_stream_id = checkpoint_stream_id.unwrap_or(""),
        checkpoint_seq,
        challenge_stream_id = liveness.challenge_stream_id().unwrap_or(""),
        challenge_seq = liveness.challenge_seq(),
        baseline_ready = replica.baseline_ready(),
        resync_latched = replica.repair_latched(),
        repair_attempts = liveness.repair_attempts(),
        socket_generation = owner.map(|token| token.socket_generation),
        process_epoch = owner.map(|token| token.process_epoch.as_str()).unwrap_or(""),
        domain_generation = owner.map(|token| token.domain_generation),
        "foreground terminal stall"
    );
}
