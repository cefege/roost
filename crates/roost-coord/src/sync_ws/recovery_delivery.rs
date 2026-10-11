//! Durable replay admission paced by terminal hydration and queue capacity.
//! Called by backfill for both log rows and the held live tail.
//! Socket progress wakes the producer; one prepared frame survives each wait.

use roost_proto::SyncDomain;

use super::ack_window::MAX_UNACKED_BYTES;
use super::domain_table::DOMAIN_MAX_QUEUED_BYTES;
use super::driver::{Delivery, SyncLink, now_ms};
use super::feed::FeedFrame;
use super::retained_frame::OwnedFrame;

pub(super) async fn deliver_recovery_frame(
    link: &SyncLink,
    frame: FeedFrame,
    generation: u64,
) -> bool {
    let (frame, meta) = frame.into_parts();
    let mut pending = OwnedFrame::of_frame(frame, SyncDomain::Terminal, generation);
    let estimated_bytes = pending.conservative_bytes();
    if estimated_bytes > DOMAIN_MAX_QUEUED_BYTES.min(MAX_UNACKED_BYTES) {
        return false;
    }
    let mut waiting = false;
    loop {
        // Register before checking capacity so a drain between the check and
        // the await cannot strand recovery until an unrelated client frame.
        let progress = link.recovery_progress();
        tokio::pin!(progress);
        progress.as_mut().enable();
        {
            let mut state = link.lock();
            if state.close.is_some() || state.replay.is_aborted() {
                return false;
            }
            let Delivery::V2(session) = &mut state.delivery else {
                return false;
            };
            if session.domain_generation(SyncDomain::Terminal) != Some(generation) {
                return false;
            }
            match session.enqueue_recovery_frame(pending, &meta, estimated_bytes, now_ms()) {
                Ok(()) => {
                    drop(state);
                    if waiting {
                        tracing::info!(
                            event = "sync-ws",
                            action = "recovery_resumed",
                            generation,
                            "durable replay resumed after socket progress"
                        );
                    }
                    link.wake_socket();
                    return true;
                }
                Err(frame) => {
                    pending = frame;
                    if !waiting {
                        tracing::info!(
                            event = "sync-ws",
                            action = "recovery_paused",
                            generation,
                            "durable replay is waiting for terminal hydration or queue capacity"
                        );
                        waiting = true;
                    }
                }
            }
        }
        progress.await;
    }
}
