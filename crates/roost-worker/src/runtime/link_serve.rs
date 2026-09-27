//! One dial's socket life: the forced hello, then the select that keeps the
//! link serving until something ends it. Called by [`super::link_loop`] once
//! per dial, and by nothing else.
//!
//! It is its own file because the select is the only place in the worker where
//! the link is owned by a loop rather than by a call, and a reader looking for
//! "what ends a link" should find that in one place rather than interleaved
//! with the drain's rules.
//!
//! Three things end a link, and the difference between them is the whole reason
//! this process exists. A stop, which is somebody's decision. Silence past the
//! staleness timeout, which is a coordinator that died behind its front door
//! while the socket stayed ESTABLISHED — on 2026-07-11 that ran for seven hours
//! with every spawn failing `worker not connected`. And a barrier that cannot
//! leave the snapshot stage, which is a build gap and is torn down so it shows
//! up in the ladder instead of looking healthy.

use std::sync::Arc;
use std::time::Instant;

use roost_protocol::wire::coord_worker::CoordWorkerUpstream;

use crate::link_barrier::{Action, Barrier};
use crate::link_dial::Link;

use super::link_loop::{DRAIN_TICK, LinkLoop, SNAPSHOT_STARVATION};
use super::stop::{LinkEnd, StopSignal};

/// Run one dial's socket life to its end.
pub(super) async fn serve(
    loop_state: &mut LinkLoop,
    mut link: Link,
    stop: &mut StopSignal,
) -> LinkEnd {
    if let Some(end) = force_hello(loop_state, &mut link).await {
        return end;
    }
    // Bound outside the loop state so no branch of the select holds a borrow of
    // it while another branch mutates it.
    let wake = Arc::clone(&loop_state.wake);
    let mut ticker = tokio::time::interval(DRAIN_TICK);
    loop {
        // A FRESH observer each pass. `requested` takes `&mut self` for the
        // whole future, so holding the caller's signal across the select would
        // borrow the loop state for as long as the arm is pending.
        let mut stop_observer = stop.clone();
        tokio::select! {
            biased;
            reason = stop_observer.requested() => return LinkEnd::Stopped(reason),
            // `tick` yields an Instant, and `() = ` is only valid for a future
            // that outputs `()`; a wildcard is the form for any other output.
            _ = ticker.tick() => {
                if let Some(end) = on_tick(loop_state, &mut link).await { return end; }
            }
            () = wake.notified() => {
                if let Some(end) = on_tick(loop_state, &mut link).await { return end; }
            }
            incoming = link.recv() => {
                let now = Instant::now();
                match incoming {
                    None => return LinkEnd::Closed,
                    Some(Err(reason)) => return LinkEnd::FrameError(reason),
                    Some(Ok(message)) => {
                        loop_state.policy.note_downstream(now);
                        if let Some(end) = super::link_drain::on_frame(loop_state, message) {
                            return end;
                        }
                    }
                }
            }
        }
    }
}

/// The one forced first write.
///
/// The socket has just opened, so its buffer is empty and this is the only frame
/// that may go out before the coordinator says hello-ack. Everything after it is
/// gated on that acknowledgement, because the acknowledgement is what makes this
/// socket generation exist at the coordinator.
async fn force_hello(loop_state: &mut LinkLoop, link: &mut Link) -> Option<LinkEnd> {
    match loop_state.pump.on_open() {
        Action::SendHello => {}
        other => {
            tracing::error!(
                ?other,
                "an open link did not ask for the hello; the barrier is wrong"
            );
            return Some(LinkEnd::HelloFailed(
                "the barrier did not ask for the hello".to_string(),
            ));
        }
    }
    // `process_epoch` is wired for real: the boot config has minted it and
    // `LinkLoopState` already carries it, and v2 sent it, so omitting it was a
    // parity gap rather than a missing feature.
    //
    // `capabilities` is the remaining half and stays UNIMPLEMENTED. An empty
    // vec encodes to zero bytes, because a proto3 repeated field with no
    // entries is absent — so the bytes on the wire are identical to a build
    // that has no such field, and this change is a no-op for a coordinator
    // rather than a newly-incompatible hello. The list is W-2's: advertising a
    // capability the worker cannot yet serve is a worse failure than admitting
    // none, and every `browser_commands::Deps` implementation is still a test
    // fake.
    let hello = CoordWorkerUpstream::Hello {
        worker_fp: loop_state.identity.worker_fp.clone(),
        version: loop_state.identity.version.clone(),
        capabilities: Vec::new(),
        process_epoch: loop_state.identity.process_epoch.clone(),
        trace_id: None,
    };
    let bytes = match loop_state.wire.encode_upstream(&hello) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::error!(%error, "the hello did not encode");
            return Some(LinkEnd::HelloFailed(error.to_string()));
        }
    };
    if let Err(error) = link.send(bytes).await {
        tracing::warn!(%error, "the hello did not reach the coordinator");
        return Some(LinkEnd::WriteFailed(error.to_string()));
    }
    tracing::info!(
        barrier = ?loop_state.pump.barrier(),
        "the hello is the only forced first write"
    );
    None
}

/// The drain, the staleness test, and the snapshot-starvation bound.
async fn on_tick(loop_state: &mut LinkLoop, link: &mut Link) -> Option<LinkEnd> {
    if let Some(end) = super::link_drain::drain(loop_state, link).await {
        return Some(end);
    }
    let now = Instant::now();
    if loop_state.policy.stale_check_due(now) && loop_state.policy.is_stale() {
        let silent = loop_state.policy.silent_for(now);
        tracing::warn!(
            silent_ms = u64::try_from(silent.as_millis()).unwrap_or(u64::MAX),
            "the coordinator link has gone silent; forcing it closed"
        );
        return Some(LinkEnd::Stale { silent });
    }
    snapshot_starvation(loop_state, now)
}

/// Whether the barrier has been stuck at the snapshot stage for too long.
///
/// No provider is a build gap, not a network fault, so re-dialling would not
/// fix it — and a link that never leaves `snapshot` is not stale by any measure
/// the watchdog uses, because the coordinator keeps pinging it. Tearing it down
/// is the only thing that makes the condition visible in the ladder, and a
/// visible failure beats a healthy-looking link that carries nothing.
fn snapshot_starvation(loop_state: &mut LinkLoop, now: Instant) -> Option<LinkEnd> {
    if loop_state.pump.barrier() != Barrier::Snapshot {
        loop_state.snapshot_since = None;
        return None;
    }
    let since = *loop_state.snapshot_since.get_or_insert(now);
    let waited = now.saturating_duration_since(since);
    if waited < SNAPSHOT_STARVATION {
        return None;
    }
    tracing::error!(
        "the barrier cannot leave the snapshot stage, so this link will never carry live traffic"
    );
    Some(LinkEnd::SnapshotStarved { waited })
}
