//! The sweep: one pass over every deadline the client owns.
//!
//! This is the only way time reaches the client, which is what makes the sweep
//! EXHAUSTIVE rather than merely periodic. There is no timer in this crate, so
//! there is no other place a deadline could fire — a host that runs this on an
//! interval runs all of them, and a host that forgets to cannot silently lose
//! one.
//!
//! This is also where a pane's identity and the id its authority holds meet: an
//! outbound command names the pane, and the wire names the id. Every publisher
//! below resolves one from the other through `TerminalSession::wire_view_id`,
//! because after a promotion they are two different strings and only one of them
//! is a lease.
//!
//! The deadlines, and what each one is for:
//!
//! - the recovery cursor write, debounced so a burst of a thousand events costs
//!   one write;
//! - the chunked-snapshot stall, at `roost_protocol`'s shared boundary, because a
//!   partial that has not advanced will never complete;
//! - the resync retry, once a heartbeat per generation, so one gap is one request
//!   rather than a storm;
//! - the candidate baseline deadline, so a staged direct attempt that never
//!   answers releases the views it published;
//! - the view heartbeat, so a pane that has gone quiet releases the session's
//!   minimum size after the park grace instead of pinning it forever;
//! - the agent-status acknowledgement write, and the retirement of the released
//!   occupants this profile has already been told about;
//! - the foreground liveness probe and its proof deadline, because a pane that
//!   stopped painting has no other watchdog.
//!
//! Depends on `effect`, `store` and `terminal`; called only by `handle_event`.

mod liveness;
mod repair;

use crate::effect::{DirectCommand, Effect, SyncCommand};
use crate::store::Store;
use crate::store::view_rotation;
use crate::sync::SyncDomain;
use crate::terminal::TerminalToken;
use crate::terminal::token::TerminalTransport;
use crate::terminal::view::ViewIntent;

pub(crate) use repair::request_candidate_repair_if_due;
pub use repair::request_repair_if_due;

/// One pass over every deadline.
pub fn handle_sweep(store: &mut Store, now_ms: u64, out: &mut Vec<Effect>) {
    // The cursor first: it is the one deadline whose work is a single write, and
    // doing it here rather than per event is what makes a burst cheap.
    if let Some(event_id) = store.sync.watermark.take_pending() {
        out.push(Effect::PersistWatermark { event_id });
    }

    // Closes whose undo window ran out owe their session a kill.
    crate::handle_close_kill::issue_due_kills(store, now_ms, out);

    // The acknowledgement ledger second, and for the same reason: one write per
    // sweep no matter how many rows the reader looked at since the last one.
    if store.agent_seen_dirty {
        store.agent_seen_dirty = false;
        out.push(Effect::PersistAgentSeen {
            encoded: store.agent_seen.encode(),
        });
    }

    // Retiring is here rather than on the frame because retirement is a
    // CONVERSATION between two reports and an acknowledgement: a released
    // occupant's row outlives the frame that released it until this profile has
    // been told, and no single report can know that.
    if !store
        .agent_status
        .retire_spent_released(&store.agent_seen)
        .is_empty()
    {
        store.note_change();
    }

    let session_ids: Vec<String> = store.terminal.keys().cloned().collect();
    for session_id in session_ids {
        let dropped = store
            .terminal_mut_if_present(&session_id)
            .is_some_and(|replica| replica.sweep(now_ms));
        if dropped {
            // `sweep` reports that it dropped a stalled partial, which clears
            // the in-flight flag and may latch a repair behind it.
            store.note_change();
        }
        request_repair_if_due(store, &session_id, now_ms, out);
        request_candidate_repair_if_due(store, &session_id, now_ms, out);
        liveness::sweep_foreground_liveness(store, &session_id, now_ms, out);
        republish_due_views(store, &session_id, now_ms, out);
    }

    crate::handle_terminal::sweep_candidate_deadlines(store, now_ms, out);

    // Held input. A batch that waited out its admission is REFUSED, not sent:
    // nothing left the client, so refusing it cannot lose a keystroke.
    let mut refused_any = false;
    for outcome in store.input.sweep_held(now_ms) {
        refused_any = true;
        tracing::info!(
            target: "terminal",
            input_seq = outcome.input_seq(),
            "held terminal input refused at its admission timeout"
        );
    }

    if refused_any {
        store.note_change();
    }

    // The direct carriers' own deadlines last, so a grant that expires on this
    // pass is reported as expired and its retry is armed from the same instant
    // the rest of the client used.
    store.direct.sweep(now_ms, out);
}

/// Republish every view whose heartbeat is due.
///
/// The heartbeat is not politeness. A dropped transport parks a view, and a
/// parked view stops constraining geometry after the park grace — so a pane that
/// has gone quiet has to keep saying it is there, or it releases the session's
/// minimum size without anyone noticing it had left.
fn republish_due_views(store: &mut Store, session_id: &str, now_ms: u64, out: &mut Vec<Effect>) {
    let due: Vec<String> = store
        .terminal(session_id)
        .map(|replica| {
            replica
                .views()
                .values()
                .filter(|view| view.intent != ViewIntent::Unpublish && view.heartbeat_due(now_ms))
                .map(|view| view.view_id.clone())
                .collect()
        })
        .unwrap_or_default();
    for view_id in due {
        publish_view(store, session_id, &view_id, now_ms, out);
    }
}

/// Where a session's next view command goes: its elected direct route, else the
/// live Sync socket once the terminal domain is ready, else nowhere (v2
/// `terminal-stream-publication.ts` `terminalPublicationTarget`).
///
/// The target, not the replica, is the authority. A replica that has never seen
/// a frame has no generation, and a view that waited for one would never be
/// published — so the coordinator would never stream it the frame that binds it.
fn publication_target(store: &Store, session_id: &str) -> Option<TerminalToken> {
    if let Some(route) = store.routes.route(session_id) {
        return Some(route.token.clone());
    }
    if !store.sync.domain_is_ready(SyncDomain::Terminal) {
        return None;
    }
    store.sync.terminal_token()
}

/// Republish every open view of every replica, on the target that now carries
/// it. Called when the terminal domain turns ready (v2 `retargetSession`): a
/// view opened while no route could carry it was left unsent, and nothing else
/// would send it before the next heartbeat.
pub(crate) fn republish_open_views(store: &mut Store, now_ms: u64, out: &mut Vec<Effect>) {
    let open: Vec<(String, String)> = store
        .terminal
        .iter()
        .flat_map(|(session_id, replica)| {
            replica
                .views()
                .values()
                .filter(|view| view.intent != ViewIntent::Unpublish)
                .map(|view| (session_id.clone(), view.view_id.clone()))
                .collect::<Vec<_>>()
        })
        .collect();
    for (session_id, view_id) in open {
        publish_view(store, &session_id, &view_id, now_ms, out);
    }
}

/// Publish a view's current intent on the session's publication target, and
/// mark it awaited. A target the replica is not fenced to re-fences it first
/// (v2 `terminal-stream-view-commands.ts`: the view's generation becomes the
/// target's token before the command is sent).
pub fn publish_view(
    store: &mut Store,
    session_id: &str,
    view_id: &str,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    // A session mid-rotation holds an id that belonged to a carrier that is gone.
    // Publishing it to the coordinator would take a lease on a handle the direct
    // worker still has in its own table, and the two would then disagree about
    // who is serving the pane.
    if view_rotation::is_pending(store, session_id) {
        return;
    }
    let Some(token) = publication_target(store, session_id) else {
        // No route can carry it: left unsent, as v2 leaves it pending. The
        // terminal domain turning ready republishes it, and so does the heartbeat.
        return;
    };
    publish_view_on_token(store, &token, session_id, view_id, now_ms, out);
}

/// Publish one view's current intent on a NAMED token, and mark it awaited.
///
/// `pub` inside the crate because the Sync rotation knows its answer before the
/// target rule is consulted: the rule is "whichever route is elected", and this
/// is the one caller whose route has already been established to be Sync.
pub(crate) fn publish_view_on_token(
    store: &mut Store,
    token: &TerminalToken,
    session_id: &str,
    view_id: &str,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    let Some(replica) = store.terminal(session_id) else {
        return;
    };
    let Some(view) = replica.view(view_id) else {
        return;
    };
    let (wire_view_id, intent, revision) = (view.wire_view_id.clone(), view.intent, view.revision);
    if let Some(replica) = store.terminal_mut_if_present(session_id)
        && replica.bind_generation(token)
    {
        store.note_change();
        tracing::info!(target: "terminal", session_id, "replica fenced to the publication target");
    }
    // Awaited on the generation THIS TRANSPORT stamps its answers with, which is
    // the domain generation on Sync and the socket generation on a direct carrier.
    let generation = token.view_answer_generation();
    if let Some(replica) = store.terminal_mut_if_present(session_id) {
        replica.mark_view_published(view_id, generation, now_ms);
    }
    send_intent_with(
        store,
        token,
        session_id,
        &wire_view_id,
        intent,
        revision,
        out,
    );
}

/// Send one intent without marking it awaited. Hide and close use this: they are
/// not commands the lease waits on, so there is nothing to await an answer to.
/// `revision` is the one the view recorded for this intent (`TerminalView`).
pub fn send_intent(
    store: &mut Store,
    session_id: &str,
    view_id: &str,
    intent: ViewIntent,
    revision: u64,
    out: &mut Vec<Effect>,
) {
    let Some(replica) = store.terminal(session_id) else {
        return;
    };
    // The token comes from the replica, exactly as it does for a publish: an
    // intent sent on a token the replica is not fenced to is a command for a
    // route that no longer exists.
    let Some(token) = replica.generation().cloned() else {
        return;
    };
    let Some(wire_view_id) = replica.wire_view_id(view_id).map(str::to_string) else {
        return;
    };
    send_intent_with(
        store,
        &token,
        session_id,
        &wire_view_id,
        intent,
        revision,
        out,
    );
}

/// Send one intent for a wire id the caller already holds, naming the pane it
/// belongs to no longer.
///
/// Only a closing pane needs this, and only because its record is already gone:
/// `close_view` removes the view, so the id the authority holds has to have been
/// read before the removal. Every other caller has the record and uses
/// `send_intent`.
pub(crate) fn send_intent_for_wire(
    store: &mut Store,
    session_id: &str,
    wire_view_id: &str,
    intent: ViewIntent,
    revision: u64,
    out: &mut Vec<Effect>,
) {
    let Some(token) = store
        .terminal(session_id)
        .and_then(|replica| replica.generation().cloned())
    else {
        return;
    };
    send_intent_with(
        store,
        &token,
        session_id,
        wire_view_id,
        intent,
        revision,
        out,
    );
}

fn send_intent_with(
    store: &Store,
    token: &TerminalToken,
    session_id: &str,
    wire_view_id: &str,
    intent: ViewIntent,
    revision: u64,
    out: &mut Vec<Effect>,
) {
    if token.transport == TerminalTransport::Sync {
        out.push(Effect::SendSync(SyncCommand::TerminalView {
            session_id: session_id.to_string(),
            view_id: wire_view_id.to_string(),
            intent,
            revision,
            token: token.clone(),
        }));
    } else if store.routes.route_matches(session_id, token) {
        out.push(Effect::SendDirect {
            token: token.clone(),
            command: DirectCommand::View {
                session_id: session_id.to_string(),
                view_id: wire_view_id.to_string(),
                intent,
                revision,
            },
        });
    }
}
