//! The repair request: one rule, aimed at whichever replica owes it.
//!
//! Split out of `handle_sweep` because the two are read for different reasons.
//! This is what you read when a gap was latched and the question is "who asks
//! for a baseline, and about which view"; the sweep is what you read when time
//! passed.
//!
//! The candidate arm exists because a staged replica has its OWN latched gaps
//! and its own views. Asking the canonical would ask a different authority,
//! about a different view, for a baseline of a stream the canonical is not even
//! expecting.

use crate::effect::{DirectCommand, Effect, SyncCommand};
use crate::store::Store;
use crate::store::view_rotation;
use crate::terminal::TerminalSession;
use crate::terminal::TerminalToken;
use crate::terminal::token::TerminalTransport;

/// Ask for a fresh baseline when the latch says one is due.
///
/// `pub` because the view-state path calls it directly: an accepted view answer
/// that installs a new stream is exactly the moment the replica needs to know
/// whether it already owes a request, and routing that through a sweep would
/// delay the repair by a heartbeat.
pub fn request_repair_if_due(
    store: &mut Store,
    session_id: &str,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    // Nothing goes out for a session whose id is mid-rotation: the repair would
    // be sent on the generation of a carrier that is already gone, and the
    // route-loss path has issued the one request that is actually deliverable.
    if view_rotation::is_pending(store, session_id) {
        return;
    }
    let Some(token) = store
        .terminal(session_id)
        .and_then(|replica| replica.generation().cloned())
    else {
        return;
    };
    let request = store
        .terminal(session_id)
        .and_then(|replica| repair_request(replica, &token, session_id, now_ms));
    let Some(request) = request else {
        return;
    };
    if let Some(replica) = store.terminal_mut_if_present(session_id) {
        replica.mark_repair_sent(&token, now_ms);
        // A request that goes out with no proof deadline behind it owes one: the
        // accepted delta that cleared this gap's latch timestamp proved the lane,
        // not the hole.
        replica.begin_scoped_repair(&token, now_ms);
    }
    out.push(request);
}

/// The same latch rule, aimed at a STAGED candidate rather than the canonical.
///
/// A candidate that is missing its baseline is a candidate whose view already
/// carries an expected stream, so a latched gap behind it has somewhere to be
/// repaired to — and the replica that holds the latch is the staged one, whose
/// views are the candidate's own wire ids.
pub(crate) fn request_candidate_repair_if_due(
    store: &mut Store,
    session_id: &str,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    let Some(token) = store.routes.staged_token(session_id) else {
        return;
    };
    let request = store
        .routes
        .staged_replica_mut(session_id)
        .and_then(|replica| repair_request(replica, &token, session_id, now_ms));
    let Some(request) = request else {
        return;
    };
    if let Some(replica) = store.routes.staged_replica_mut(session_id) {
        replica.mark_repair_sent(&token, now_ms);
    }
    out.push(request);
}

/// The resync command one replica owes, or `None` when it owes none.
fn repair_request(
    replica: &TerminalSession,
    token: &TerminalToken,
    session_id: &str,
    now_ms: u64,
) -> Option<Effect> {
    if !replica.repair_latched() || !replica.repair_due(token, now_ms) {
        return None;
    }
    scoped_resync(replica, token, session_id)
}

/// The scoped resync command a replica's current position asks for, on the
/// token that would carry it.
///
/// The latch gates above decide WHETHER one is owed; this builds it, and it is
/// the one construction in the client. A liveness challenge asks for the same
/// baseline — same view, same stream, same checkpoint — so it is built here
/// rather than a second time with a second set of rules.
pub(crate) fn scoped_resync(
    replica: &TerminalSession,
    token: &TerminalToken,
    session_id: &str,
) -> Option<Effect> {
    let wire_view_id = replica.repair_view()?.wire_view_id.clone();
    // And it names the stream it is a baseline OF. With no expected stream there
    // is nothing to name, so nothing is sent and nothing is marked sent: the
    // view acceptance that installs the stream re-enters here.
    let position = replica.resync_position()?;
    Some(match token.transport {
        TerminalTransport::Sync => Effect::SendSync(SyncCommand::TerminalResync {
            session_id: session_id.to_string(),
            view_id: wire_view_id,
            stream_id: position.stream_id,
            grid_epoch: position.grid_epoch,
            seq: position.seq,
            token: token.clone(),
        }),
        TerminalTransport::Loopback | TerminalTransport::Peer => Effect::SendDirect {
            token: token.clone(),
            command: DirectCommand::Resync {
                session_id: session_id.to_string(),
                view_id: wire_view_id,
                stream_id: position.stream_id,
                grid_epoch: position.grid_epoch,
                seq: position.seq,
            },
        },
    })
}
