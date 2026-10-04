//! What goes on the coordinator link, and in what order. Called by
//! [`super::link_serve`] from its tick and for every frame the [`crate::uplink`]
//! carries, by [`super::link_downstream`] for every answer a downstream frame
//! produces, and by [`super::link_loop`] when a producer offers a durable event
//! (v2 `apps/worker/src/transport/coord-link-outbox.ts`'s gates and
//! `maybeNotifyWritable`).
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

use std::sync::Arc;
use std::time::Instant;

use roost_protocol::wire::agent_status::AgentStatusUpdate;
use roost_protocol::wire::coord_worker::CoordWorkerUpstream;

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
    /// An agent status: v2 `drainQueues` writes these after the events and the
    /// writable notification, ahead of the controls.
    AgentStatus { bytes: Vec<u8> },
    /// The repair a refused cell sink produced when told the link is writable,
    /// written as one run ahead of the queued controls (v2 `notifyingWritable`).
    Repair(Vec<Pending>),
}

/// Admit one frame the uplink carried, on its lane, under v2's outbox gates:
/// compact metadata only once negotiated, raw PTY metadata only while not.
pub(super) fn admit_uplink(loop_state: &mut LinkLoop, frame: CoordWorkerUpstream) {
    match frame {
        CoordWorkerUpstream::TerminalMetadata(metadata) => {
            if !loop_state.terminal_metadata_negotiated {
                tracing::debug!(channel = %metadata.channel_id, "terminal metadata dropped: this link did not negotiate it");
                return;
            }
            if let Err(error) = loop_state.send_terminal_metadata(metadata.channel_id, &metadata) {
                tracing::warn!(%error, "terminal metadata was refused by the link");
            }
        }
        CoordWorkerUpstream::Binary(binary) => {
            if loop_state.terminal_metadata_negotiated {
                tracing::trace!(channel = %binary.channel_id, "raw metadata dropped: compact metadata is negotiated");
                return;
            }
            let frame = CoordWorkerUpstream::Binary(binary);
            admit_to_lane(loop_state, &frame, Lane::RawMetadata, "raw-metadata");
        }
        CoordWorkerUpstream::AgentStatus(frame) => {
            let status = AgentStatusUpdate {
                common: frame.status.common,
                active: frame.status.active,
            };
            if let Err(error) = loop_state.send_agent_status(&status) {
                tracing::warn!(%error, "an agent status was refused by the link");
            }
        }
        control => push_upstream(loop_state, &control, control.kind()),
    }
}

/// Take what the barrier and the outbox allow, and put it on the socket.
pub(super) async fn drain(loop_state: &mut LinkLoop, link: &mut Link) -> Option<LinkEnd> {
    // BOTH HOOKS RUN BEFORE `next_write`, and both are here rather than in the
    // frame handler for the same reason the delete is: this is the one place
    // that already owns the tick and the socket, so a durable row retired here
    // is a row that leaves before the next write rather than after it.
    let retired = loop_state.apply_durable_acks().await;
    if retired > 0 {
        tracing::info!(
            retired,
            "the coordinator's acknowledgements retired durable rows"
        );
    }
    // v2 `coordLinkSink`: rows the sink wrote reach the pump before anything is
    // chosen, and a snapshot the barrier asked for is numbered by the outbox.
    loop_state.sync_durable_rows().await;
    loop_state.authorise_snapshot().await;
    let moved = loop_state.move_cell_frames_into();
    if moved > 0 {
        tracing::debug!(
            moved,
            "cell frames moved onto the coordinator link's terminal lane"
        );
    }
    let now = Instant::now();
    let mut written = 0u64;
    let mut notified = false;
    while let Some(next) = next_write(loop_state, now, &mut notified) {
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
                // The mirror keeps its head until the coordinator acknowledges
                // the sequence (`link_downstream`), because a reconnect
                // re-releases an unacknowledged row and the mirror must still
                // hold it to write it again under its own sequence.
                link.send(bytes).await.map(|()| seq)
            }
            NextWrite::Snapshot { bytes } => link.send(bytes).await.map(|()| 0),
            NextWrite::Queued(frame) => link.send(frame.bytes).await.map(|()| 0),
            // Committed only once the socket took it, so a failed write stays
            // pending for the next link (v2 keeps it until `tryWrite` succeeds).
            NextWrite::AgentStatus { bytes } => link.send(bytes).await.map(|()| {
                loop_state.agent_statuses.commit_written();
                0
            }),
            NextWrite::Repair(frames) => write_repair(link, frames).await,
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
    loop_state.decide_replay_barrier();
    None
}

async fn write_repair(
    link: &mut Link,
    frames: Vec<Pending>,
) -> Result<u64, crate::link_dial::DialError> {
    for frame in frames {
        link.send(frame.bytes).await?;
    }
    Ok(0)
}

/// v2 `maybeNotifyWritable`: a cell sink that refused a frame is told the link
/// can take cells again only once the link is live, no durable event is left to
/// write, and the terminal lane has drained. The session layer builds its repair
/// inside `on_writable`, and what it produced is returned to be written next.
fn notify_writable(loop_state: &mut LinkLoop, now: Instant) -> Option<Vec<Pending>> {
    if !loop_state.durable.is_empty() || loop_state.outbox.lane_len(Lane::Terminal) > 0 {
        return None;
    }
    let sink = Arc::clone(loop_state.cell_sink.as_ref()?);
    if !sink.take_writable_owed() {
        return None;
    }
    tracing::debug!("the coordinator link is writable again for a cell sink that was refused");
    if let Some(owners) = loop_state.dispatcher.owners() {
        owners.lifecycle.on_writable();
    }
    let mut repair = crate::outbox::Outbox::default();
    sink.drain_into(&mut repair, now);
    Some(repair.drain_all(now))
}

/// What goes on the socket next, or nothing — v2 `drainQueues`' order.
///
/// The pong first, even before the link is live, so a ping is answered while
/// replay runs. Then a durable write the barrier has released, because the
/// coordinator's acknowledgement of that sequence is what the barrier is
/// waiting on. After that only a live barrier releases the lanes, and a cell
/// sink still owed a writable notification holds the controls: the repair the
/// notification produces leads any queued reply, once per pass
/// (`coord-link-repair-order.test.ts`: opened → full → RPC). Agent statuses go
/// after that and ahead of the lanes (v2: events → agent statuses → controls).
fn next_write(loop_state: &mut LinkLoop, now: Instant, notified: &mut bool) -> Option<NextWrite> {
    if let Some(pong) = loop_state.outbox.take_from(Lane::Liveness) {
        return Some(NextWrite::Queued(pong));
    }
    if let Some(authorised) = loop_state.authorised.take() {
        return Some(match authorised {
            Authorised::Durable(seq) => NextWrite::Durable { seq },
            Authorised::Snapshot(bytes) => NextWrite::Snapshot { bytes },
        });
    }
    if !loop_state.pump.barrier().allows_live_traffic() {
        return None;
    }
    if loop_state
        .cell_sink
        .as_ref()
        .is_some_and(|sink| sink.writable_owed())
    {
        if let Some(cells) = loop_state.outbox.take_from(Lane::Terminal) {
            return Some(NextWrite::Queued(cells));
        }
        if *notified {
            return None;
        }
        let repair = notify_writable(loop_state, now)?;
        *notified = true;
        return Some(NextWrite::Repair(repair));
    }
    if let Some(bytes) = loop_state.agent_statuses.next_bytes() {
        return Some(NextWrite::AgentStatus {
            bytes: bytes.to_vec(),
        });
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
                // A reconnect re-releases the same row under the same sequence.
                if let Some(previous) = frame.seq
                    && previous != seq
                {
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
        // The sequence is the outbox's, and drawing it is async: the drain
        // authorises the snapshot (`link_loop::durable_sync`).
        Action::WriteSnapshot => {
            loop_state.snapshot_wanted = true;
            loop_state.wake();
        }
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

/// Encode, admit to its lane and wake: the pong to the liveness lane, every
/// other answer to the control lane (v2 `send`). A frame that does not fit is
/// reported, never silently dropped: the caller is a liveness reply or an
/// answer, and both are worse absent than refused.
pub(super) fn push_upstream(loop_state: &mut LinkLoop, frame: &CoordWorkerUpstream, label: &str) {
    let lane = if matches!(frame, CoordWorkerUpstream::Pong { .. }) {
        Lane::Liveness
    } else {
        Lane::Control
    };
    admit_to_lane(loop_state, frame, lane, label);
}

fn admit_to_lane(loop_state: &mut LinkLoop, frame: &CoordWorkerUpstream, lane: Lane, label: &str) {
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
    if let Err(error) = loop_state.outbox.admit(lane, bytes, label, Instant::now()) {
        tracing::error!(label, %error, "an upstream frame did not fit the outbox");
        return;
    }
    loop_state.wake();
}
