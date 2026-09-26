//! Full-before-delta: can this frame extend the replica that is installed?
//!
//! Pure. It reads the expectation and the canonical frame and answers, and it
//! mutates the canonical only once every rule has passed. Nothing here knows
//! about renderers, repairs, sockets, or clocks, which is why a staged direct
//! candidate and the canonical replica can share it verbatim.
//!
//! Ported from `apps/web/src/client/terminal-stream/terminal-stream-frame-fold.ts`,
//! with the rules and their sources in `docs/phase4-client-contract.md` §6.

use roost_proto::{PbCellGridChunk, PbCellGridFrame};
use roost_protocol::cell::frame_chunks::encoded_cell_grid_frame_size;
use roost_protocol::cell::{
    CELL_GRID_PART_MAX_BYTES, CellGridFrame, apply_delta, normalize_cell_grid_frame,
    proto_to_cell_frame,
};

/// The state a frame is folded against: what the replica expects, and what it
/// currently holds.
///
/// `canonical_chunk_in_flight` is the assembler's `activeSnapshotId !== null`
/// from v2. It is a plain `bool` here because the assembler itself is the
/// authority on what is in flight; the core only needs to know that something
/// is, and a second source for "which snapshot" would be a second thing to keep
/// correct.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FoldTarget {
    /// The stream id the replica is fenced to, or `None` before any view has
    /// installed an expectation.
    pub expected_stream_id: Option<String>,
    /// The pane's effective column count.
    pub effective_cols: u32,
    /// The pane's effective row count.
    pub effective_rows: u32,
    /// The installed frame, or `None` when no complete full has been accepted.
    pub canonical: Option<CellGridFrame>,
    /// True once a complete full for the CURRENT stream is installed.
    pub baseline_ready: bool,
    /// True while a chunked baseline is partially assembled.
    pub canonical_chunk_in_flight: bool,
}

/// Why a frame was refused. Every member is a contract string the repair ladder
/// and the incident log both read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameFoldFailure {
    /// A full that is not a complete baseline for the current expectation.
    InvalidFull,
    /// A full that would move the replica backwards.
    FullConflict,
    /// A delta that does not exactly continue the installed frame.
    DeltaUnfollowed,
    /// A delta that passed the fences but whose rows would not fold.
    DeltaFoldRejected,
}

impl FrameFoldFailure {
    /// The wire spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidFull => "invalid_full",
            Self::FullConflict => "full_conflict",
            Self::DeltaUnfollowed => "delta_unfollowed",
            Self::DeltaFoldRejected => "delta_fold_rejected",
        }
    }
}

/// What folding one frame did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameFoldOutcome {
    /// A complete baseline replaced the replica. No payload: the frame is in
    /// `target.canonical` already, and carrying a second copy would deep-clone a
    /// whole grid on every baseline for a field no caller reads.
    Full,
    /// A delta extended the replica. `delta` is what arrived, for a renderer's
    /// per-frame work; the replica after the fold is `target.canonical`, which
    /// is borrowed rather than copied — a delta arrives many times a second and
    /// a whole-grid copy on that path is the one allocation this module can
    /// most afford not to make.
    Delta { delta: CellGridFrame },
    /// Nothing changed. The replica is exactly as it was, and the caller owes
    /// one latched repair.
    Invalid { reason: FrameFoldFailure },
}

/// Decode one bounded wire frame without touching a replica.
///
/// `assembled` relaxes exactly one check: the part ceiling. A chunked baseline
/// is SUPPOSED to exceed one part — that is why it was chunked — so re-applying
/// `CELL_GRID_PART_MAX_BYTES` to the assembled product would refuse every large
/// terminal. Every other bound is the assembler's, and has already been applied
/// part by part.
pub fn decode_wire_frame(
    frame: &PbCellGridFrame,
    assembled: bool,
) -> Result<CellGridFrame, String> {
    if !assembled && encoded_cell_grid_frame_size(frame) > CELL_GRID_PART_MAX_BYTES {
        return Err("terminal frame exceeded the encoded part ceiling".to_string());
    }
    proto_to_cell_frame(frame).map_err(|error| error.to_string())
}

/// Decode a chunk part, for the same admission the wire frame path applies.
///
/// A part is bounded by `CELL_GRID_PART_MAX_BYTES` at the assembler, not here,
/// so this only needs to establish the session and stream the part names — the
/// replica is what decides whether the part is relevant at all.
pub fn decode_chunk_part(chunk: &PbCellGridChunk) -> Option<&PbCellGridFrame> {
    chunk.part.as_option()
}

/// True when a frame is a complete baseline for the current expectation.
///
/// Every clause is a way a "full" can be a lie, and each one has bitten:
///
/// - `base_seq == 0`: a full extends nothing, so a non-zero base is a delta
///   wearing a full's flag.
/// - dimensions equal the PANE's, not merely the sender's: the authority
///   computes a minimum across views, and a client that accepted a grid its
///   pane cannot show would then keep folding deltas against the wrong `rows`.
/// - `viewport_rows[i].index == i` for every row: the renderer addresses rows by
///   index, and a mis-numbered full paints the wrong line in the wrong place.
/// - `scrollback_append` empty: a full is not also an append.
/// - history contiguous from `sb_base` up to `scrollback_total`: a gap in the
///   window means an absolute row index re-aliases at the scrollback cap, and the
///   reader cannot tell which line it is looking at.
pub fn valid_full(target: &FoldTarget, frame: &CellGridFrame) -> bool {
    if !frame.full
        || frame.base_seq != 0
        || frame.viewport_rows.len() != frame.rows as usize
        || !frame.scrollback_append.is_empty()
    {
        return false;
    }
    let Some(expected) = target.expected_stream_id.as_deref() else {
        // Nothing has installed an expectation, so there is no grid this frame
        // can be complete FOR. Admitting it would let any stream's baseline
        // become the canonical one.
        return false;
    };
    if frame.stream_id != expected
        || frame.cols != target.effective_cols
        || frame.rows != target.effective_rows
    {
        return false;
    }
    for (index, row) in frame.viewport_rows.iter().enumerate() {
        if row.index as usize != index {
            return false;
        }
    }
    // `sb_base` and `scrollback_total` are `u64` because a history index keeps
    // counting past any `u32` a grid could hold; the row index is `u32` because a
    // row is a position IN a grid. Widening the row, never narrowing the window:
    // a narrowing comparison would silently accept a window past 2^32.
    let mut history_index = frame.sb_base;
    for row in &frame.scrollback_rows {
        if u64::from(row.index) != history_index || u64::from(row.index) >= frame.scrollback_total {
            return false;
        }
        history_index += 1;
    }
    history_index == frame.scrollback_total
}

/// True when a full may replace the installed frame.
///
/// A snapshot request issued on generation A can still be answered on generation
/// B. Accepting that answer would undo B, so a full is refused when it moves the
/// sequence backwards, and refused when it re-states the same sequence with a
/// different epoch, geometry, or alt-screen occupancy — two grids claiming one
/// sequence number.
pub fn full_follows_canonical(canonical: Option<&CellGridFrame>, frame: &CellGridFrame) -> bool {
    let Some(canonical) = canonical else {
        return true;
    };
    canonical.stream_id != frame.stream_id
        || frame.seq > canonical.seq
        || (frame.seq == canonical.seq
            && frame.grid_epoch == canonical.grid_epoch
            && frame.cols == canonical.cols
            && frame.rows == canonical.rows
            && frame.alt_screen == canonical.alt_screen)
}

/// Fold one already-decoded frame onto the target, or refuse it.
///
/// The order of the two arms is load-bearing and is NOT interchangeable with a
/// helper that accepts a full: `roost_protocol::cell::apply_delta` replaces its
/// base wholesale when handed `full == true` (`diff_grid.rs:74-77`), which is
/// right for the server-side emitter and wrong for a replica. Dispatching on
/// `full` here is what puts `full_follows_canonical` in front of it.
pub fn fold(target: &mut FoldTarget, frame: CellGridFrame) -> FrameFoldOutcome {
    if frame.full {
        return fold_full(target, frame);
    }
    fold_delta(target, frame)
}

fn fold_full(target: &mut FoldTarget, mut frame: CellGridFrame) -> FrameFoldOutcome {
    if !valid_full(target, &frame) {
        return FrameFoldOutcome::Invalid {
            reason: FrameFoldFailure::InvalidFull,
        };
    }
    if !full_follows_canonical(target.canonical.as_ref(), &frame) {
        return FrameFoldOutcome::Invalid {
            reason: FrameFoldFailure::FullConflict,
        };
    }
    normalize_cell_grid_frame(&mut frame);
    target.canonical = Some(frame);
    target.baseline_ready = true;
    FrameFoldOutcome::Full
}

fn fold_delta(target: &mut FoldTarget, delta: CellGridFrame) -> FrameFoldOutcome {
    // Take the canonical rather than clone it: `apply_delta` mutates its base in
    // place, and a refused delta must leave the replica byte-identical. That it
    // does is a property of `apply_delta` validating before it mutates, and
    // `tests/terminal_epoch_fence.rs` pins it — a delta that breaks a fence must
    // not move the grid.
    let Some(mut base) = target.canonical.take() else {
        return FrameFoldOutcome::Invalid {
            reason: FrameFoldFailure::DeltaUnfollowed,
        };
    };
    let expected = target.expected_stream_id.clone().unwrap_or_default();
    // Every entry is a row of the delta-fence table in the contract.
    let refusals = [
        !target.baseline_ready,
        target.canonical_chunk_in_flight,
        delta.stream_id != expected,
        base.stream_id != expected,
        // The epoch fence. A resize mints a new grid epoch, and a delta from the
        // previous one indexes a grid that no longer exists.
        delta.grid_epoch != base.grid_epoch,
        delta.cols != target.effective_cols || delta.rows != target.effective_rows,
        delta.base_seq != base.seq,
        // `checked_add`, not `+ 1`: `base_seq` arrives from the wire, and a
        // debug-build overflow here would panic on the frame path.
        delta.base_seq.checked_add(1) != Some(delta.seq),
        delta.alt_screen != base.alt_screen,
        base.viewport_rows.len() != base.rows as usize,
        !delta.scrollback_rows.is_empty(),
    ];
    if refusals.iter().any(|refused| *refused) {
        target.canonical = Some(base);
        return FrameFoldOutcome::Invalid {
            reason: FrameFoldFailure::DeltaUnfollowed,
        };
    }
    if apply_delta(&mut base, &delta).is_none() {
        target.canonical = Some(base);
        return FrameFoldOutcome::Invalid {
            reason: FrameFoldFailure::DeltaFoldRejected,
        };
    }
    normalize_cell_grid_frame(&mut base);
    target.canonical = Some(base);
    FrameFoldOutcome::Delta { delta }
}
