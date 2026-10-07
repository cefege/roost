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

use crate::cell::frame_structure::assert_frame_structure;
use crate::cell::types::{
    CellGridFrame, CellRow, CellSpan, as_mouse_tracking, assert_cell_row_spans,
};
use crate::error::{ProtocolError, ProtocolResult};
use crate::viewport::is_terminal_uuid;

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
        mark: u32::from(row.mark & crate::cell::row_mark::KNOWN),
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
        let strings = SpanStrings {
            text: span.text.clone(),
            link_uri: span.link_uri.clone(),
            link_key: span.link_key.clone(),
        };
        spans.push(span_from_proto(span, position, strings)?);
    }
    checked_row(row.index, row.mark, spans, max_columns)
}

/// One wire row into the value model, MOVING its strings out of the message
/// rather than copying them: for a caller that owns the decoded frame.
fn cell_row_from_proto_owned(row: PbCellRow, max_columns: u32) -> ProtocolResult<CellRow> {
    let index = row.index;
    let mut spans = Vec::with_capacity(row.spans.len());
    for (position, mut span) in row.spans.into_iter().enumerate() {
        let strings = SpanStrings {
            text: std::mem::take(&mut span.text),
            link_uri: span.link_uri.take(),
            link_key: span.link_key.take(),
        };
        spans.push(span_from_proto(&span, position, strings)?);
    }
    checked_row(index, row.mark, spans, max_columns)
}

fn checked_row(
    index: u32,
    mark: u32,
    spans: Vec<CellSpan>,
    max_columns: u32,
) -> ProtocolResult<CellRow> {
    let value = CellRow::with_mark(index, Arc::from(spans), mark as u8);
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

/// One wire frame into the value model: the sequence and the structure first,
/// then every row (occupancy and link identity). The result owns its rows and
/// spans outright, so a later mutation of the decoded message cannot repaint a
/// replica that already installed this frame.
///
/// For a caller that only borrows the message — a frame dispatched by
/// reference, or one the coordinator fans out afterwards. A caller that owns
/// it uses [`proto_into_cell_frame`], which moves the strings instead.
pub fn proto_to_cell_frame(frame: &PbCellGridFrame) -> ProtocolResult<CellGridFrame> {
    assert_frame_sequence(frame)?;
    assert_frame_structure(frame)?;
    let decode_rows = |rows: &[PbCellRow], max_columns: u32| {
        rows.iter()
            .map(|row| cell_row_from_proto_bounded(row, max_columns))
            .collect::<ProtocolResult<Vec<_>>>()
    };
    Ok(CellGridFrame {
        viewport_rows: decode_rows(&frame.viewport_rows, frame.cols)?,
        scrollback_rows: decode_rows(&frame.scrollback_rows, 0)?,
        scrollback_append: decode_rows(&frame.scrollback_append, 0)?,
        stream_id: frame.stream_id.clone(),
        grid_epoch: frame.grid_epoch.clone(),
        ..frame_scalars(frame)
    })
}

/// [`proto_to_cell_frame`] for a caller that owns the message: the same rules
/// in the same order, with every string moved rather than copied.
pub fn proto_into_cell_frame(frame: PbCellGridFrame) -> ProtocolResult<CellGridFrame> {
    assert_frame_sequence(&frame)?;
    assert_frame_structure(&frame)?;
    let decode_rows = |rows: Vec<PbCellRow>, max_columns: u32| {
        rows.into_iter()
            .map(|row| cell_row_from_proto_owned(row, max_columns))
            .collect::<ProtocolResult<Vec<_>>>()
    };
    let scalars = frame_scalars(&frame);
    Ok(CellGridFrame {
        viewport_rows: decode_rows(frame.viewport_rows, frame.cols)?,
        scrollback_rows: decode_rows(frame.scrollback_rows, 0)?,
        scrollback_append: decode_rows(frame.scrollback_append, 0)?,
        stream_id: frame.stream_id,
        grid_epoch: frame.grid_epoch,
        ..scalars
    })
}

/// Every field of a decoded frame that is not a row or an owned string.
fn frame_scalars(frame: &PbCellGridFrame) -> CellGridFrame {
    CellGridFrame {
        stream_id: String::new(),
        grid_epoch: String::new(),
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
        viewport_rows: Vec::new(),
        scrollback_rows: Vec::new(),
        scrollback_append: Vec::new(),
        scrollback_total: frame.scrollback_total,
        sb_base: frame.sb_base,
        base_seq: frame.base_seq,
        seq: frame.seq,
    }
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

/// The strings a decoded span keeps, copied or moved out of the message by
/// the caller depending on whether it owns it.
struct SpanStrings {
    text: String,
    link_uri: Option<String>,
    link_key: Option<String>,
}

/// One wire span into the value model. The palette and flag fields are `u16`
/// here and `u32` on the wire, so an out-of-range value is refused rather than
/// truncated into a colour that was never emitted.
fn span_from_proto(
    span: &PbCellSpan,
    position: usize,
    strings: SpanStrings,
) -> ProtocolResult<CellSpan> {
    Ok(CellSpan {
        text: strings.text,
        fg: narrow_span_field(position, "fg", span.fg)?,
        bg: narrow_span_field(position, "bg", span.bg)?,
        flags: narrow_span_field(position, "flags", span.flags)?,
        fg_rgb: span.fg_rgb,
        bg_rgb: span.bg_rgb,
        columns: span.columns,
        link_uri: strings.link_uri,
        link_key: strings.link_key,
    })
}

/// A span's wire value the value model cannot hold, refused with its field
/// path. The path is only built for a refusal: this runs per span per frame.
fn narrow_span_field(position: usize, name: &str, value: u32) -> ProtocolResult<u16> {
    u16::try_from(value).map_err(|_| {
        ProtocolError::new(
            format!("cell_row.spans[{position}].{name}"),
            format!("{value} is outside the 16-bit palette and flag range"),
        )
    })
}
