//! Baseline and delta admission for a session's screen replica, and the
//! fan-out each admission owes the sockets watching it.
//!
//! Split out of `replica` (the port of
//! `apps/coord/src/terminal/screen/terminal-screen-hub.ts`, `acceptFull`,
//! `acceptDelta`, `installCache`) for the size cap. Every refusal here fails
//! closed and asks for a repair; a wrong grid is never served.

use std::sync::Arc;

use roost_proto::PbCellGridFrame;
use roost_protocol::cell::frame_chunk_validation::CELL_GRID_COORD_FANOUT_STAMP_MAX_ENCODED_BYTES;
use roost_protocol::cell::frame_chunk_validation::assert_cell_grid_snapshot;
use roost_protocol::cell::frame_chunks::encoded_cell_grid_frame_size;
use roost_protocol::cell::{
    CELL_GRID_PART_MAX_BYTES, CELL_GRID_SNAPSHOT_MAX_SPANS, CellGridFrame, apply_delta,
    clone_cell_grid_frame, normalize_cell_grid_frame, proto_to_cell_frame,
};
use roost_protocol::wire::SessionId;

use crate::terminal_screen::hub_fanout::ScreenEffect;
use crate::terminal_screen::hub_state::{ExpectedStream, SessionScreen};
use crate::terminal_screen::replica::ScreenHub;
use crate::terminal_screen::residency::ResidentCache;
use crate::terminal_screen::snapshot_source::cell_grid_envelope;

const CAPACITY_EXCEEDED: &str = "coordinator terminal cache capacity exceeded";

impl ScreenHub {
    /// Admit a complete authoritative baseline. A chunked one's held deltas
    /// are folded afterwards, by [`Self::replay_held_deltas`].
    pub(crate) fn accept_full(
        self: &Arc<Self>,
        session_id: &SessionId,
        screen: &mut SessionScreen,
        proto: &mut PbCellGridFrame,
        assembled: bool,
        effects: &mut Vec<ScreenEffect>,
    ) {
        let Some(expected) = screen.expected.clone() else {
            return;
        };
        if proto.stream_id != expected.stream_id {
            return;
        }
        match Self::validated_full(proto, &expected, assembled) {
            Ok((frame, spans)) => {
                self.complete_repair(screen);
                self.install_cache(
                    session_id,
                    screen,
                    frame,
                    spans,
                    proto.coord_recv_ms,
                    effects,
                );
            }
            Err(reason) => {
                tracing::warn!(session_id = %session_id, reason = %reason, "a terminal baseline was refused");
                self.retry(session_id, screen, &reason, effects);
            }
        }
    }

    /// Fold the deltas held while a chunked baseline assembled into the
    /// baseline just installed, once that baseline's own fan-out has run. A
    /// held delta whose base no longer matches the live replica is skipped,
    /// never folded: the installed full already contains everything emitted
    /// before it.
    pub(crate) fn replay_held_deltas(self: &Arc<Self>, session_id: &SessionId) {
        let mut effects = Vec::new();
        {
            let mut sessions = self.locked_sessions();
            let Some(screen) = sessions.get_mut(session_id) else {
                return;
            };
            for mut delta in screen.hold.drain() {
                let live = screen.charge.current().is_some_and(|cache| cache.valid)
                    && !screen.resync_latched;
                if !live {
                    break;
                }
                let seq = screen.charge.current().map(|cache| cache.frame.seq);
                if seq == Some(delta.base_seq) {
                    self.accept_delta(session_id, screen, &mut delta, &mut effects);
                }
            }
        }
        self.run_effects(effects);
    }

    fn validated_full(
        proto: &PbCellGridFrame,
        expected: &ExpectedStream,
        assembled: bool,
    ) -> Result<(CellGridFrame, u64), String> {
        if !assembled && encoded_cell_grid_frame_size(proto) > CELL_GRID_PART_MAX_BYTES {
            return Err("unchunked terminal full exceeds part limit".to_owned());
        }
        assert_cell_grid_snapshot(proto).map_err(|error| error.reason)?;
        if proto.cols != expected.cols || proto.rows != expected.rows {
            return Err("terminal baseline geometry does not match expected stream".to_owned());
        }
        let mut frame = proto_to_cell_frame(proto).map_err(|error| error.to_string())?;
        normalize_cell_grid_frame(&mut frame);
        let spans = viewport_spans(&frame);
        Ok((frame, spans))
    }

    /// Install a validated baseline and seed every watcher from it, degrading
    /// loudly when it does not fit the residency budget.
    fn install_cache(
        self: &Arc<Self>,
        session_id: &SessionId,
        screen: &mut SessionScreen,
        frame: CellGridFrame,
        spans: u64,
        coord_recv_ms: u64,
        effects: &mut Vec<ScreenEffect>,
    ) {
        let rows = u64::from(frame.rows);
        {
            let mut residency = self.locked_residency();
            if !residency.can_replace(&screen.charge, rows, spans) {
                // THE DEGRADE: drop what is resident and say the session is
                // unavailable, rather than admit a frame the pool cannot pay
                // for -- the path that corrupts a screen instead of blanking it.
                residency.drop_cache(&mut screen.charge);
                screen.source = None;
                tracing::error!(session_id = %session_id, rows, spans, "terminal.screen_capacity: a baseline exceeded the residency budget");
                effects.push(ScreenEffect::Unavailable {
                    session_id: session_id.clone(),
                    reason: CAPACITY_EXCEEDED.to_owned(),
                });
                return;
            }
            if !residency.replace(&mut screen.charge, frame, coord_recv_ms, rows, spans) {
                // Unsignalled, a budget refusal makes an accepted full silently
                // vanish.
                tracing::error!(session_id = %session_id, rows, spans, "terminal.screen_capacity: the pool refused an accepted baseline");
                return;
            }
        }
        screen.source = None;
        screen.resync_latched = false;
        let Some(stream_id) = screen
            .expected
            .as_ref()
            .map(|expected| expected.stream_id.clone())
        else {
            return;
        };
        effects.push(ScreenEffect::FullAccepted {
            session_id: session_id.clone(),
            stream_id: stream_id.clone(),
        });
        let watchers = self.locked_sockets().watchers_of(session_id);
        if watchers.is_empty() {
            return;
        }
        let Some(source) = self.seed_source(session_id, screen, effects) else {
            return;
        };
        for watcher in watchers {
            effects.push(ScreenEffect::Seed {
                watcher,
                session_id: session_id.clone(),
                stream_id: stream_id.clone(),
                source: Arc::clone(&source),
            });
        }
    }

    /// Fold one delta into the current replica and fan it out. Any failure
    /// marks the cache invalid and latches one repair rather than skipping
    /// the delta: a skipped delta is a permanently wrong grid that still looks
    /// complete.
    pub(crate) fn accept_delta(
        self: &Arc<Self>,
        session_id: &SessionId,
        screen: &mut SessionScreen,
        proto: &mut PbCellGridFrame,
        effects: &mut Vec<ScreenEffect>,
    ) {
        let Some(expected) = screen.expected.clone() else {
            return;
        };
        if proto.stream_id != expected.stream_id {
            return;
        }
        let Some(cache) = screen.charge.current().filter(|cache| cache.valid) else {
            let reason = "terminal delta arrived before a complete baseline";
            self.latch(session_id, screen, reason, effects);
            return;
        };
        let folded = match Self::folded_delta(proto, cache, &expected) {
            Ok(folded) => folded,
            Err(reason) => return self.refuse_delta(session_id, screen, reason, effects),
        };
        let rows = u64::from(folded.rows);
        let spans = viewport_spans(&folded);
        let generation = {
            let mut residency = self.locked_residency();
            let replaced = residency.can_replace(&screen.charge, rows, spans)
                && residency.replace(&mut screen.charge, folded, proto.coord_recv_ms, rows, spans);
            replaced
                .then(|| screen.charge.current().map(|cache| cache.generation))
                .flatten()
        };
        let Some(generation) = generation else {
            return self.refuse_delta(session_id, screen, CAPACITY_EXCEEDED, effects);
        };
        screen.source = None;
        screen.resync_latched = false;
        let frame = Arc::new(cell_grid_envelope(proto.clone()));
        // The fallback full is planned only for a socket that refuses the
        // delta: planning one per delta would cost a snapshot per keystroke.
        for watcher in self.locked_sockets().watchers_of(session_id) {
            effects.push(ScreenEffect::Delta {
                watcher,
                session_id: session_id.clone(),
                stream_id: expected.stream_id.clone(),
                frame: Arc::clone(&frame),
                generation,
            });
        }
    }

    fn folded_delta(
        proto: &PbCellGridFrame,
        cache: &ResidentCache,
        expected: &ExpectedStream,
    ) -> Result<CellGridFrame, &'static str> {
        if proto.full
            || proto.base_seq != cache.frame.seq
            || proto.seq != proto.base_seq.saturating_add(1)
            || proto.grid_epoch != cache.frame.grid_epoch
            || proto.cols != expected.cols
            || proto.rows != expected.rows
        {
            return Err("terminal delta does not follow the canonical baseline");
        }
        let ceiling = CELL_GRID_PART_MAX_BYTES - CELL_GRID_COORD_FANOUT_STAMP_MAX_ENCODED_BYTES;
        if encoded_cell_grid_frame_size(proto) > ceiling {
            return Err("terminal delta leaves no fanout stamp headroom");
        }
        let delta = proto_to_cell_frame(proto)
            .map_err(|_| "terminal delta cannot be folded into baseline")?;
        let mut folded = clone_cell_grid_frame(&cache.frame);
        apply_delta(&mut folded, &delta).ok_or("terminal delta cannot be folded into baseline")?;
        normalize_cell_grid_frame(&mut folded);
        if viewport_spans(&folded) > u64::from(CELL_GRID_SNAPSHOT_MAX_SPANS) {
            return Err("terminal cache span limit exceeded");
        }
        Ok(folded)
    }

    fn refuse_delta(
        self: &Arc<Self>,
        session_id: &SessionId,
        screen: &mut SessionScreen,
        reason: &str,
        effects: &mut Vec<ScreenEffect>,
    ) {
        if let Some(cache) = screen.charge.current.as_mut() {
            cache.valid = false;
        }
        tracing::warn!(session_id = %session_id, reason, "a terminal delta was refused");
        self.latch(session_id, screen, reason, effects);
    }
}

/// Resident spans are counted over the viewport only, even for an older
/// worker's history-bearing frame (`countTerminalScreenCacheSpans`). Saturating
/// rather than `as u64`: a wrapped count would pass the ceiling.
fn viewport_spans(frame: &CellGridFrame) -> u64 {
    frame
        .viewport_rows
        .iter()
        .map(|row| u64::try_from(row.spans.len()).unwrap_or(u64::MAX))
        .fold(0_u64, u64::saturating_add)
}
