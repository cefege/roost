//! The cell value model's wire boundary: a `CellGridFrame` in, a
//! `PbCellGridFrame` out, and the reverse for the receiving side.
//!
//! The worker fills frames of the internal value types, coord stamps a session
//! and fans them out, and every front end decodes them back. This module is the
//! only place those two shapes meet, which is what keeps the renderer and the
//! pure fold logic free of protobuf.
//!
//! Column occupancy and hyperlink identity are validated HERE, on both
//! directions: a row that does not carry its occupancy paints every wide glyph
//! one column short, and a link without its run identity cannot be clicked or
//! grouped. Both faults are refused at the boundary rather than downstream.

use std::sync::Arc;

use roost_proto::{PbCellGridFrame, PbCellRow, PbCellSpan};

use crate::cell::types::{
    CellGridFrame, CellRow, CellSpan, as_mouse_tracking, assert_cell_row_spans,
};
use crate::error::{ProtocolError, ProtocolResult};
use crate::viewport::{TerminalGeometry, assert_terminal_geometry, is_terminal_uuid};

/// One internal span as the wire spells it.
fn span_to_proto(span: &CellSpan) -> PbCellSpan {
    PbCellSpan {
        text: span.text.clone(),
        fg: u32::from(span.fg),
        bg: u32::from(span.bg),
        flags: u32::from(span.flags),
        fg_rgb: span.fg_rgb,
        bg_rgb: span.bg_rgb,
        columns: span.columns,
        link_uri: span.link_uri.clone(),
        link_key: span.link_key.clone(),
        __buffa_unknown_fields: Default::default(),
    }
}

/// One internal row as the wire spells it.
pub fn cell_row_to_proto(row: &CellRow) -> PbCellRow {
    PbCellRow {
        index: row.index,
        spans: row.spans.iter().map(span_to_proto).collect(),
        __buffa_unknown_fields: Default::default(),
    }
}

/// One wire row into the value model, checked against `max_columns`.
///
/// `max_columns == 0` skips the width bound, because a retained scrollback line
/// keeps its write-time width and no current-grid bound applies to it. The
/// TypeScript adapter returned the decoded row object itself; the two shapes
/// are distinct types here, so the transfer is a conversion and validation
/// rides along with it.
pub fn cell_row_from_proto_bounded(row: &PbCellRow, max_columns: u32) -> ProtocolResult<CellRow> {
    let mut spans = Vec::with_capacity(row.spans.len());
    for (position, span) in row.spans.iter().enumerate() {
        spans.push(span_from_proto(span, position)?);
    }
    let value = CellRow {
        index: row.index,
        spans: Arc::from(spans),
    };
    assert_cell_row_spans(&value, max_columns)?;
    Ok(value)
}

/// One wire row into the value model with no width bound.
pub fn cell_row_from_proto(row: &PbCellRow) -> ProtocolResult<CellRow> {
    cell_row_from_proto_bounded(row, 0)
}

/// One whole frame out to the wire. The identity and sequence rules run on the
/// caller's values first, the structure rules on what was actually built, so a
/// frame that cannot be painted is never handed to the encoder.
pub fn cell_frame_to_proto(
    frame: &CellGridFrame,
    session_id: &str,
) -> ProtocolResult<PbCellGridFrame> {
    assert_frame_identity(
        &frame.stream_id,
        &frame.grid_epoch,
        frame.full,
        frame.base_seq,
        frame.seq,
    )?;
    let proto = PbCellGridFrame {
        session_id: session_id.to_owned(),
        stream_id: frame.stream_id.clone(),
        grid_epoch: frame.grid_epoch.clone(),
        cols: frame.cols,
        rows: frame.rows,
        cursor_row: frame.cursor_row,
        cursor_col: frame.cursor_col,
        cursor_visible: frame.cursor_visible,
        alt_screen: frame.alt_screen,
        full: frame.full,
        viewport_rows: frame.viewport_rows.iter().map(cell_row_to_proto).collect(),
        scrollback_rows: frame
            .scrollback_rows
            .iter()
            .map(cell_row_to_proto)
            .collect(),
        scrollback_append: frame
            .scrollback_append
            .iter()
            .map(cell_row_to_proto)
            .collect(),
        scrollback_total: frame.scrollback_total,
        seq: frame.seq,
        sb_base: frame.sb_base,
        cursor_keys_app: frame.cursor_keys_app,
        bracketed_paste: frame.bracketed_paste,
        pty_out_ms: 0,
        worker_emit_ms: 0,
        coord_recv_ms: 0,
        coord_fanout_ms: 0,
        mouse_tracking: frame.mouse_tracking.into(),
        mouse_sgr: frame.mouse_sgr,
        focus_events: frame.focus_events,
        base_seq: frame.base_seq,
        __buffa_unknown_fields: Default::default(),
    };
    assert_frame_structure(&proto)?;
    Ok(proto)
}

/// One wire frame into the value model: rows first (occupancy and link
/// identity), then the sequence, then the structure. The result owns its rows
/// and spans outright, so a later mutation of the decoded message cannot
/// repaint a replica that already installed this frame.
pub fn proto_to_cell_frame(frame: &PbCellGridFrame) -> ProtocolResult<CellGridFrame> {
    let mut viewport_rows = Vec::with_capacity(frame.viewport_rows.len());
    for row in &frame.viewport_rows {
        viewport_rows.push(cell_row_from_proto_bounded(row, frame.cols)?);
    }
    let scrollback_rows = frame
        .scrollback_rows
        .iter()
        .map(|row| cell_row_from_proto_bounded(row, 0))
        .collect::<ProtocolResult<Vec<_>>>()?;
    let scrollback_append = frame
        .scrollback_append
        .iter()
        .map(|row| cell_row_from_proto_bounded(row, 0))
        .collect::<ProtocolResult<Vec<_>>>()?;
    assert_frame_sequence(frame)?;
    assert_frame_structure(frame)?;
    Ok(CellGridFrame {
        stream_id: frame.stream_id.clone(),
        grid_epoch: frame.grid_epoch.clone(),
        cols: frame.cols,
        rows: frame.rows,
        cursor_row: frame.cursor_row,
        cursor_col: frame.cursor_col,
        cursor_visible: frame.cursor_visible,
        alt_screen: frame.alt_screen,
        cursor_keys_app: frame.cursor_keys_app,
        bracketed_paste: frame.bracketed_paste,
        mouse_tracking: as_mouse_tracking(frame.mouse_tracking),
        mouse_sgr: frame.mouse_sgr,
        focus_events: frame.focus_events,
        full: frame.full,
        viewport_rows,
        scrollback_rows,
        scrollback_append,
        scrollback_total: frame.scrollback_total,
        sb_base: frame.sb_base,
        base_seq: frame.base_seq,
        seq: frame.seq,
    })
}

/// The stream and sequence identity every frame carries, whoever built it: a
/// full baseline names base_seq=0, and a delta continues its predecessor by
/// exactly one.
fn assert_frame_identity(
    stream_id: &str,
    grid_epoch: &str,
    full: bool,
    base_seq: u64,
    seq: u64,
) -> ProtocolResult<()> {
    if !is_terminal_uuid(stream_id) {
        return Err(ProtocolError::new(
            "cell.stream_id",
            format!("cell stream_id is not a UUID: {stream_id:?}"),
        ));
    }
    if grid_epoch.is_empty() {
        return Err(ProtocolError::new(
            "cell.grid_epoch",
            "cell grid_epoch is empty",
        ));
    }
    if seq < 1 {
        return Err(ProtocolError::new(
            "cell.seq",
            format!("cell sequence {base_seq}->{seq} is outside the safe positive range"),
        ));
    }
    if full {
        if base_seq != 0 {
            return Err(ProtocolError::new(
                "cell.base_seq",
                format!("full cell frame has nonzero base_seq {base_seq}"),
            ));
        }
        return Ok(());
    }
    if seq != base_seq + 1 {
        return Err(ProtocolError::new(
            "cell.seq",
            format!("cell delta seq {seq} does not follow base_seq {base_seq}"),
        ));
    }
    Ok(())
}

/// The sequence half of the identity rules, for a frame that arrived on the wire.
fn assert_frame_sequence(frame: &PbCellGridFrame) -> ProtocolResult<()> {
    assert_frame_identity(
        &frame.stream_id,
        &frame.grid_epoch,
        frame.full,
        frame.base_seq,
        frame.seq,
    )
}

/// What a frame claims to contain must be what it contains: legal geometry, one
/// row per viewport index, and — for a full — history that covers
/// `[sb_base, scrollback_total)` exactly, in order.
fn assert_frame_structure(frame: &PbCellGridFrame) -> ProtocolResult<()> {
    let geometry = TerminalGeometry {
        cols: frame.cols,
        rows: frame.rows,
    };
    assert_terminal_geometry(&geometry).map_err(|error| error.within("cell_frame"))?;
    let mut seen = std::collections::HashSet::new();
    for row in &frame.viewport_rows {
        if row.index >= frame.rows {
            return Err(ProtocolError::new(
                format!("cell_frame.viewport_rows[{}].index", row.index),
                format!(
                    "cell viewport row {} is outside 0..{}",
                    row.index,
                    frame.rows - 1
                ),
            ));
        }
        if !seen.insert(row.index) {
            return Err(ProtocolError::new(
                format!("cell_frame.viewport_rows[{}].index", row.index),
                format!("cell viewport row {} occurs more than once", row.index),
            ));
        }
    }
    if !frame.full {
        if !frame.scrollback_rows.is_empty() {
            return Err(ProtocolError::new(
                "cell_frame.scrollback_rows",
                "cell delta cannot carry scrollback_rows",
            ));
        }
        return Ok(());
    }
    if seen.len() != frame.rows as usize {
        return Err(ProtocolError::new(
            "cell_frame.viewport_rows",
            format!(
                "full cell frame has {} of {} required viewport rows",
                seen.len(),
                frame.rows
            ),
        ));
    }
    if !frame.scrollback_append.is_empty() {
        return Err(ProtocolError::new(
            "cell_frame.scrollback_append",
            "full cell frame cannot carry scrollback_append",
        ));
    }
    if frame.sb_base > frame.scrollback_total
        || frame.scrollback_rows.len() as u64 != frame.scrollback_total - frame.sb_base
    {
        return Err(ProtocolError::new(
            "cell_frame.scrollback_rows",
            format!(
                "full cell frame history does not cover [{}, {}) exactly",
                frame.sb_base, frame.scrollback_total
            ),
        ));
    }
    for (offset, row) in frame.scrollback_rows.iter().enumerate() {
        let expected = frame.sb_base + offset as u64;
        if u64::from(row.index) != expected {
            return Err(ProtocolError::new(
                format!("cell_frame.scrollback_rows[{offset}].index"),
                format!("full cell frame history row {expected} is missing or out of order"),
            ));
        }
    }
    Ok(())
}

/// One wire span into the value model. The palette and flag fields are `u16`
/// here and `u32` on the wire, so an out-of-range value is refused rather than
/// truncated into a colour that was never emitted.
fn span_from_proto(span: &PbCellSpan, position: usize) -> ProtocolResult<CellSpan> {
    let field = format!("cell_row.spans[{position}]");
    Ok(CellSpan {
        text: span.text.clone(),
        fg: narrow(&format!("{field}.fg"), span.fg)?,
        bg: narrow(&format!("{field}.bg"), span.bg)?,
        flags: narrow(&format!("{field}.flags"), span.flags)?,
        fg_rgb: span.fg_rgb,
        bg_rgb: span.bg_rgb,
        columns: span.columns,
        link_uri: span.link_uri.clone(),
        link_key: span.link_key.clone(),
    })
}

/// A wire value the value model cannot hold, refused with its field path.
fn narrow(field: &str, value: u32) -> ProtocolResult<u16> {
    u16::try_from(value).map_err(|_| {
        ProtocolError::new(
            field,
            format!("{value} is outside the 16-bit palette and flag range"),
        )
    })
}
