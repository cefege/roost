//! What goes on the coordinator link, and in what order. Called by
//! [`super::link_serve`] from its tick and from every downstream frame, and by
//! [`super::link_loop`] when a producer offers a durable event.
//!
//! This is the file where the pump's decisions become bytes. The pump says which
//! one durable event may go out and the outbox says which lane comes next; this
//! file is the only place that turns either of those into a `send`, which is why
//! the mirror, the authorisation slot and the send all live together here.
//!
//! The rule that decides the order, once more because it is the one that is easy
//! to break by accident: a session's `opened` event must reach the coordinator
//! before that session's first cells. A cell frame for a session nobody has been
//! told about is a frame the browser cannot place, and the failure looks like a
//! terminal that never paints rather than like an ordering bug.

use std::time::Instant;

use roost_protocol::wire::coord_worker::{CoordWorkerDownstream, CoordWorkerUpstream};
use tokio_tungstenite::tungstenite::Message;

use crate::link_barrier::Action;
use crate::link_dial::Link;
use crate::outbox::{Lane, Pending};

use super::link_loop::{Authorised, LinkLoop};
use super::stop::LinkEnd;

/// One frame the writer is about to put on the socket.
#[derive(Debug)]
enum NextWrite {
    /// The barrier released the durable event at the head of the mirror.
    Durable { seq: u64 },
    /// The snapshot, which the coordinator acknowledges from the same sequence
    /// space a durable event uses.
    Snapshot { bytes: Vec<u8> },
    /// A control, terminal or raw-metadata frame, in lane order.
    Queued(Pending),
}

/// Handle one downstream frame. Returns an end only for a stop.
pub(super) fn on_frame(loop_state: &mut LinkLoop, message: Message) -> Option<LinkEnd> {
    let Message::Binary(bytes) = message else {
        // This link carries binary protobuf. A text frame is something else
        // answering, and answering it is how a worker ends up speaking a second
        // protocol on the socket that carries its events.
        tracing::warn!("a non-binary frame arrived on the coordinator link; ignoring it");
        return None;
    };
    let frame = match loop_state.wire.decode_downstream(&bytes) {
        Ok(frame) => frame,
        Err(error) => {
            tracing::warn!(%error, "a downstream frame did not decode");
            return None;
        }
    };
    match frame {
        CoordWorkerDownstream::HelloAck { .. } => {
            tracing::info!("the coordinator acknowledged the hello");
            let action = loop_state.pump.on_hello_ack();
            apply_to(loop_state, action);
        }
        CoordWorkerDownstream::Ping { ts, .. } => {
            // Admitted to the control lane, so the barrier governs when it goes
            // out like every other frame. The coordinator reads the round trip
            // as liveness, and a pong held behind a barrier that has not opened
            // is the barrier working, not a lost heartbeat.
            push_upstream(
                loop_state,
                &CoordWorkerUpstream::Pong { ts, trace_id: None },
                "pong",
            );
        }
        CoordWorkerDownstream::BrowserCommand { request_id, .. } => {
            // `browser_commands::dispatch` is built and tested, but its `Deps`
            // needs four collaborators that do not exist yet: the session
            // layer, the retained grid, the scrollback scanner and the
            // terminal capture recorder. Constructing it needs implementations
            // of those traits, and passing anything else would be a stub
            // answering commands it never ran.
            //
            // What is not acceptable is silence: a command that is neither
            // executed nor refused hangs the browser's request with no error
            // anywhere. It is refused explicitly and correlated by
            // `request_id`, which is the one thing the coordinator can route a
            // failure back on.
            tracing::warn!(
                request_id,
                "a browser command arrived with no session layer to run it"
            );
            push_upstream(
                loop_state,
                &CoordWorkerUpstream::RpcError {
                    request_id,
                    message: super::link_loop::NO_SESSION_LAYER_REFUSAL.to_string(),
                    trace_id: None,
                },
                "browser-command-refusal",
            );
        }
    }
    None
}

/// Take what the barrier and the outbox allow, and put it on the socket.
pub(super) async fn drain(loop_state: &mut LinkLoop, link: &mut Link) -> Option<LinkEnd> {
    let now = Instant::now();
    let mut written = 0u64;
    while let Some(next) = next_write(loop_state, now) {
        let sent = match next {
            NextWrite::Durable { seq } => {
                let Some(frame) = loop_state.durable.front() else {
                    tracing::error!(
                        seq,
                        "the writer holds no durable event for the released sequence"
                    );
                    return Some(LinkEnd::WriteFailed(
                        "the durable mirror lost its head".to_string(),
                    ));
                };
                let bytes = frame.bytes.clone();
                // The mirror keeps its head until the socket accepts the frame,
                // because `Link::send` consumes the buffer whether it succeeds or
                // not. Popping first would drop a durable event the coordinator
                // never received, with no sequence left to replay it under.
                match link.send(bytes).await {
                    Ok(()) => {
                        let released = loop_state.durable.pop_front();
                        loop_state.durable_bytes = loop_state
                            .durable_bytes
                            .saturating_sub(released.map_or(0, |frame| frame.bytes.len()));
                        Ok(seq)
                    }
                    Err(error) => Err(error),
                }
            }
            NextWrite::Snapshot { bytes } => link.send(bytes).await.map(|()| 0),
            NextWrite::Queued(frame) => link.send(frame.bytes).await.map(|()| 0),
        };
        match sent {
            Ok(seq) => {
                written += 1;
                tracing::trace!(seq, "wrote a frame to the coordinator link");
            }
            Err(error) => {
                // A durable event is not lost: the pump still holds its sequence
                // and the next dial replays it. A control, terminal or raw frame
                // is, and that is the design — the grid re-describes the screen
                // and the coordinator re-issues its own commands, while a raw
                // frame is volatile by contract.
                return Some(LinkEnd::WriteFailed(error.to_string()));
            }
        }
    }
    if written > 0 {
        tracing::debug!(written, "the outbox drained to the coordinator link");
    }
    None
}

/// What goes on the socket next, or nothing.
///
/// A durable write the barrier has released goes first, ahead of anything the
/// outbox holds, because the coordinator's acknowledgement of that sequence is
/// what the barrier is waiting on and nothing else may sit between them. After
/// that, only a live barrier releases the lanes, in the outbox's own order.
fn next_write(loop_state: &mut LinkLoop, now: Instant) -> Option<NextWrite> {
    if let Some(authorised) = loop_state.authorised.take() {
        return Some(match authorised {
            Authorised::Durable(seq) => NextWrite::Durable { seq },
            Authorised::Snapshot(bytes) => NextWrite::Snapshot { bytes },
        });
    }
    if !loop_state.pump.barrier().allows_live_traffic() {
        return None;
    }
    loop_state.outbox.drain_one(now).map(NextWrite::Queued)
}

/// Act on what the barrier asked for.
pub(super) fn apply_to(loop_state: &mut LinkLoop, action: Action) {
    match action {
        Action::SendHello => {
            // Only `on_open` produces this, and the socket is written directly
            // there. Reaching it here would mean the barrier believes a durable
            // event outran the hello, which it forbids.
            tracing::error!("the barrier asked for the hello outside the open transition");
        }
        Action::WriteDurable { seq } => match loop_state.durable.front_mut() {
            None => {
                tracing::error!(
                    seq,
                    "the barrier released a durable event the writer does not hold"
                );
            }
            Some(frame) => {
                if let Some(previous) = frame.seq {
                    tracing::error!(
                        seq,
                        previous,
                        "two durable events claimed one pump sequence"
                    );
                }
                frame.seq = Some(seq);
                loop_state.authorised = Some(Authorised::Durable(seq));
            }
        },
        Action::WriteSnapshot => authorise_snapshot(loop_state),
        Action::IgnoredAck { seq } => {
            // Not an error. A duplicate or stale acknowledgement is normal on a
            // reconnect, and refusing one would turn a benign duplicate into an
            // outage.
            tracing::debug!(
                seq,
                "the coordinator acknowledged a sequence that was not in flight"
            );
        }
        Action::Wait => {}
    }
}

fn authorise_snapshot(loop_state: &mut LinkLoop) {
    match loop_state.snapshot.snapshot() {
        Ok(bytes) => {
            loop_state.snapshot_since = None;
            tracing::info!(bytes = bytes.len(), "publishing the worker snapshot");
            loop_state.authorised = Some(Authorised::Snapshot(bytes));
        }
        Err(error) => {
            tracing::warn!(%error, "the barrier cannot leave the snapshot stage yet");
        }
    }
}

/// Encode, admit and wake. A frame that does not fit is reported, never
/// silently dropped: the caller is a liveness reply or an error reply, and both
/// are worse absent than refused.
fn push_upstream(loop_state: &mut LinkLoop, frame: &CoordWorkerUpstream, label: &str) {
    let bytes = match loop_state.wire.encode_upstream(frame) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::error!(
                kind = frame.kind(),
                %error,
                "an upstream frame did not encode"
            );
            return;
        }
    };
    if let Err(error) = loop_state
        .outbox
        .admit(Lane::Control, bytes, label, Instant::now())
    {
        tracing::error!(label, %error, "an upstream frame did not fit the outbox");
        return;
    }
    loop_state.wake();
}
