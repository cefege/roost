//! Baseline and delta admission for a session's screen replica.
//!
//! Split out of `replica` because the hub plus its state sat over the 400-line
//! cap. These are the rules that decide whether a frame becomes the replica or
//! a refusal, and they read without the socket and hold machinery around them.

use roost_proto::PbCellGridFrame;
use roost_protocol::cell::frame_chunk_validation::CELL_GRID_COORD_FANOUT_STAMP_MAX_ENCODED_BYTES;
use roost_protocol::cell::frame_chunk_validation::assert_cell_grid_snapshot;
use roost_protocol::cell::frame_chunks::encoded_cell_grid_frame_size;
use roost_protocol::cell::{
    CELL_GRID_PART_MAX_BYTES, CELL_GRID_SNAPSHOT_MAX_SPANS, apply_delta, clone_cell_grid_frame,
    normalize_cell_grid_frame, proto_to_cell_frame,
};
use roost_protocol::wire::SessionId;

use crate::terminal_screen::byte_hub::PublishOutcome;
use crate::terminal_screen::replica::{ExpectedStream, ScreenHub, SessionScreen};
use crate::terminal_screen::residency::ResidentCache;

impl ScreenHub {
    /// Admit a complete authoritative baseline, installing it as the replica.
    ///
    /// Every failure here latches the resync and never falls back to serving
    /// the previous grid: a wrong baseline is worse than a missing one, because
    /// the browser cannot tell which it is looking at.
    pub(crate) fn accept_full(
        &self,
        session_id: &SessionId,
        screen: &mut SessionScreen,
        proto: &mut PbCellGridFrame,
        assembled: bool,
    ) -> PublishOutcome {
        let Some(expected) = screen.expected.clone() else {
            return PublishOutcome::NoExpectedStream {
                session_id: session_id.clone(),
            };
        };
        let refusal = Self::validate_full(proto, &expected, assembled);
        if let Some(reason) = refusal {
            screen.resync_latched = true;
            tracing::warn!(session_id = %session_id, reason, "a terminal baseline was refused");
            return PublishOutcome::Published {
                session_id: session_id.clone(),
            };
        }
        let coord_recv_ms = proto.coord_recv_ms;
        let Ok(mut frame) = proto_to_cell_frame(proto) else {
            screen.resync_latched = true;
            return PublishOutcome::Published {
                session_id: session_id.clone(),
            };
        };
        normalize_cell_grid_frame(&mut frame);
        let spans = u64::from(match assert_cell_grid_snapshot(proto) {
            Ok(stats) => stats.spans,
            Err(error) => {
                screen.resync_latched = true;
                tracing::warn!(session_id = %session_id, reason = %error.reason, "a terminal baseline failed snapshot validation");
                return PublishOutcome::Published {
                    session_id: session_id.clone(),
                };
            }
        });
        self.install_cache(session_id, screen, frame, spans, coord_recv_ms);
        let live_seq = screen.charge.current().map(|cache| cache.frame.seq);
        let live =
            screen.charge.current().is_some_and(|cache| cache.valid) && !screen.resync_latched;
        let replayed = screen.hold.drain(live_seq, live);
        for delta in replayed {
            let mut delta = delta;
            self.accept_delta(session_id, screen, &mut delta);
        }
        PublishOutcome::Published {
            session_id: session_id.clone(),
        }
    }

    fn validate_full(
        proto: &PbCellGridFrame,
        expected: &ExpectedStream,
        assembled: bool,
    ) -> Option<&'static str> {
        if proto.stream_id != expected.stream_id {
            return Some("terminal baseline is for another stream");
        }
        if !assembled && encoded_cell_grid_frame_size(proto) > CELL_GRID_PART_MAX_BYTES {
            return Some("unchunked terminal full exceeds part limit");
        }
        if proto.cols != expected.cols || proto.rows != expected.rows {
            return Some("terminal baseline geometry does not match expected stream");
        }
        None
    }

    /// Install an already-validated baseline, degrading loudly if it does not
    /// fit the residency budget.
    fn install_cache(
        &self,
        session_id: &SessionId,
        screen: &mut SessionScreen,
        frame: roost_protocol::cell::CellGridFrame,
        spans: u64,
        coord_recv_ms: u64,
    ) {
        let rows = u64::from(frame.rows);
        let Ok(mut residency) = self.residency.lock() else {
            return;
        };
        if !residency.can_replace(&screen.charge, rows, spans) {
            // THE DEGRADE. Drop what is resident, say the session is
            // unavailable, and raise exactly one capacity signal -- rather than
            // admitting a frame the pool cannot pay for, which is the path that
            // corrupts a screen instead of blanking it.
            let _ = residency.drop_cache(&mut screen.charge);
            const REASON: &str = "coordinator terminal cache capacity exceeded";
            self.sink.unavailable(session_id, REASON);
            tracing::error!(
                session_id = %session_id,
                rows,
                spans,
                "a terminal screen exceeded the coordinator residency budget and was dropped"
            );
            return;
        }
        if !residency.replace(&mut screen.charge, frame, coord_recv_ms, rows, spans) {
            // Unsignalled, a budget refusal makes an accepted full silently
            // vanish; the sink's `unavailable` is for a session that had a
            // screen, and this one still does.
            tracing::error!(
                session_id = %session_id,
                rows,
                spans,
                "the coordinator terminal residency pool refused an accepted baseline"
            );
            return;
        }
        drop(residency);
        screen.resync_latched = false;
        let stream_id = screen
            .expected
            .as_ref()
            .map(|expected| expected.stream_id.clone());
        if let Some(stream_id) = stream_id {
            self.sink.full_accepted(session_id, &stream_id);
        }
    }

    /// Fold one delta into the current replica. Any failure marks the cache
    /// invalid and latches the resync rather than skipping the delta, because a
    /// skipped delta is a permanently wrong grid that still looks complete.
    fn accept_delta(
        &self,
        session_id: &SessionId,
        screen: &mut SessionScreen,
        proto: &mut PbCellGridFrame,
    ) {
        let Some(expected) = screen.expected.clone() else {
            return;
        };
        let Some(cache) = screen.charge.current.as_ref() else {
            screen.resync_latched = true;
            return;
        };
        if !cache.valid {
            screen.resync_latched = true;
            return;
        }
        let refusal = Self::validate_delta(proto, cache, &expected);
        if let Some(reason) = refusal {
            if let Some(cache) = screen.charge.current.as_mut() {
                cache.valid = false;
            }
            screen.resync_latched = true;
            tracing::warn!(session_id = %session_id, reason, "a terminal delta was refused");
            return;
        }
        let coord_recv_ms = proto.coord_recv_ms;
        let Ok(delta) = proto_to_cell_frame(proto) else {
            if let Some(cache) = screen.charge.current.as_mut() {
                cache.valid = false;
            }
            screen.resync_latched = true;
            return;
        };
        let mut folded = clone_cell_grid_frame(&cache.frame);
        if apply_delta(&mut folded, &delta).is_none() {
            if let Some(cache) = screen.charge.current.as_mut() {
                cache.valid = false;
            }
            screen.resync_latched = true;
            return;
        }
        normalize_cell_grid_frame(&mut folded);
        // A span COUNT against a span count, not a byte count against a byte
        // count, so the pool and the ceiling are in the same unit. Saturating
        // rather than `as u64`: a grid this large is already refused, and a
        // wrapped count would pass the ceiling.
        let spans = folded
            .viewport_rows
            .iter()
            .map(|row| u64::try_from(row.spans.len()).unwrap_or(u64::MAX))
            .sum::<u64>();
        if spans > u64::from(CELL_GRID_SNAPSHOT_MAX_SPANS) {
            if let Some(cache) = screen.charge.current.as_mut() {
                cache.valid = false;
            }
            screen.resync_latched = true;
            return;
        }
        self.install_cache(session_id, screen, folded, spans, coord_recv_ms);
    }

    fn validate_delta(
        proto: &PbCellGridFrame,
        cache: &ResidentCache,
        expected: &ExpectedStream,
    ) -> Option<&'static str> {
        if proto.stream_id != expected.stream_id {
            return Some("terminal delta is for another stream");
        }
        if proto.full
            || proto.base_seq != cache.frame.seq
            || proto.seq != proto.base_seq + 1
            || proto.grid_epoch != cache.frame.grid_epoch
            || proto.cols != expected.cols
            || proto.rows != expected.rows
        {
            return Some("terminal delta does not follow the canonical baseline");
        }
        let ceiling = CELL_GRID_PART_MAX_BYTES - CELL_GRID_COORD_FANOUT_STAMP_MAX_ENCODED_BYTES;
        if encoded_cell_grid_frame_size(proto) > ceiling {
            return Some("terminal delta leaves no fanout stamp headroom");
        }
        None
    }
}
