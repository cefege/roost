//! Exact canonical-view comparison: the ONE implementation of "are these two
//! terminal states the same state", which the worker runs between a fresh core
//! scan and its emitted-frame fold. Ports the comparison half of
//! `packages/protocol/src/terminal-capture-view.ts`; delivery representation
//! (full/baseSeq, history pages, appends) is excluded on purpose, so a sparse
//! delta and a full meaning the same screen compare equal.

use serde::{Deserialize, Serialize};

use crate::cell::{CellGridFrame, CellRow, CellSpan, row_columns, span_is_atomic};

/// Viewport-only projection of a terminal state, independent of how it
/// shipped. It BORROWS its frame: a caller that retains a view across a fold
/// retains the frame it was taken from, never a row list a later fold rewrites.
#[derive(Debug, Clone, Copy)]
pub struct TerminalCanonicalView<'a> {
    frame: &'a CellGridFrame,
}

impl TerminalCanonicalView<'_> {
    /// Dense, index-ordered viewport rows.
    pub fn viewport_rows(&self) -> &[CellRow] {
        &self.frame.viewport_rows
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TerminalCanonicalField {
    Cols,
    Rows,
    CursorRow,
    CursorCol,
    CursorVisible,
    AltScreen,
    CursorKeysApp,
    BracketedPaste,
    MouseTracking,
    MouseSgr,
    FocusEvents,
    SbBase,
    ScrollbackTotal,
    ViewportRowCount,
    RowIndex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TerminalCellField {
    Text,
    Columns,
    Fg,
    Bg,
    Flags,
    FgRgb,
    BgRgb,
    LinkUri,
    LinkKey,
    RowColumns,
}

/// First exact disagreement between two views. `row`/`column` name the painted
/// coordinate of a cell-level difference without carrying terminal text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TerminalCanonicalDifference {
    State {
        field: TerminalCanonicalField,
        left: String,
        right: String,
    },
    Row {
        row: u32,
        column: u32,
        field: TerminalCellField,
        left: String,
        right: String,
    },
}

/// Project a wire frame. Only a dense full viewport is a canonical view; a
/// sparse delta must be folded onto its baseline first, which `None` forces a
/// caller to notice instead of comparing a patch against a screen.
pub fn canonical_view_of_frame(frame: &CellGridFrame) -> Option<TerminalCanonicalView<'_>> {
    if frame.viewport_rows.len() != frame.rows as usize {
        return None;
    }
    let dense = frame
        .viewport_rows
        .iter()
        .enumerate()
        .all(|(position, row)| row.index as usize == position);
    dense.then_some(TerminalCanonicalView { frame })
}

/// The state fields, in v2's comparison order, as the strings a difference
/// reports them with.
fn state_fields(frame: &CellGridFrame) -> [(TerminalCanonicalField, String); 13] {
    use TerminalCanonicalField as Field;
    [
        (Field::Cols, frame.cols.to_string()),
        (Field::Rows, frame.rows.to_string()),
        (Field::CursorRow, frame.cursor_row.to_string()),
        (Field::CursorCol, frame.cursor_col.to_string()),
        (Field::CursorVisible, frame.cursor_visible.to_string()),
        (Field::AltScreen, frame.alt_screen.to_string()),
        (Field::CursorKeysApp, frame.cursor_keys_app.to_string()),
        (Field::BracketedPaste, frame.bracketed_paste.to_string()),
        (
            Field::MouseTracking,
            u32::from(frame.mouse_tracking).to_string(),
        ),
        (Field::MouseSgr, frame.mouse_sgr.to_string()),
        (Field::FocusEvents, frame.focus_events.to_string()),
        (Field::SbBase, frame.sb_base.to_string()),
        (Field::ScrollbackTotal, frame.scrollback_total.to_string()),
    ]
}

/// Exact comparison: the FIRST difference in a stable order (state fields, row
/// count, then rows top-to-bottom and left-to-right), or `None` when the two
/// views are the same terminal state.
pub fn compare_canonical_views(
    left: &TerminalCanonicalView<'_>,
    right: &TerminalCanonicalView<'_>,
) -> Option<TerminalCanonicalDifference> {
    let right_state = state_fields(right.frame);
    for ((field, left_value), (_, right_value)) in
        state_fields(left.frame).into_iter().zip(right_state)
    {
        if left_value != right_value {
            return Some(TerminalCanonicalDifference::State {
                field,
                left: left_value,
                right: right_value,
            });
        }
    }
    let (left_rows, right_rows) = (left.viewport_rows(), right.viewport_rows());
    if left_rows.len() != right_rows.len() {
        return Some(TerminalCanonicalDifference::State {
            field: TerminalCanonicalField::ViewportRowCount,
            left: left_rows.len().to_string(),
            right: right_rows.len().to_string(),
        });
    }
    for (position, (left_row, right_row)) in left_rows.iter().zip(right_rows).enumerate() {
        if left_row.index != right_row.index {
            return Some(TerminalCanonicalDifference::State {
                field: TerminalCanonicalField::RowIndex,
                left: left_row.index.to_string(),
                right: right_row.index.to_string(),
            });
        }
        let row = u32::try_from(position).unwrap_or(u32::MAX);
        if let Some(difference) = compare_rows(row, &left_row.spans, &right_row.spans) {
            return Some(difference);
        }
    }
    None
}

/// Column-aligned span walk: the same painted cells over a different span
/// split are equal, so a re-coalesced fold cannot masquerade as corruption.
fn compare_rows(
    row: u32,
    left: &[CellSpan],
    right: &[CellSpan],
) -> Option<TerminalCanonicalDifference> {
    let (left_columns, right_columns) = (row_columns(left), row_columns(right));
    if left_columns != right_columns {
        return Some(TerminalCanonicalDifference::Row {
            row,
            column: left_columns.min(right_columns),
            field: TerminalCellField::RowColumns,
            left: left_columns.to_string(),
            right: right_columns.to_string(),
        });
    }
    let (left_cells, right_cells) = (expand_row(left), expand_row(right));
    for (column, (left_cell, right_cell)) in left_cells.iter().zip(&right_cells).enumerate() {
        if let Some(field) = first_cell_field_difference(left_cell, right_cell) {
            return Some(TerminalCanonicalDifference::Row {
                row,
                column: u32::try_from(column).unwrap_or(u32::MAX),
                field,
                left: describe_cell_field(left_cell, field),
                right: describe_cell_field(right_cell, field),
            });
        }
    }
    None
}

/// One grid column: an atomic span's lead column holds its whole glyph and its
/// continuation columns hold `""`, exactly as the painted grid reads.
#[derive(Debug, Clone, Copy)]
struct ExpandedCell<'a> {
    text: &'a str,
    columns: u32,
    span: &'a CellSpan,
}

fn expand_row(spans: &[CellSpan]) -> Vec<ExpandedCell<'_>> {
    let mut cells = Vec::with_capacity(row_columns(spans) as usize);
    for span in spans {
        if span_is_atomic(span) {
            cells.push(ExpandedCell {
                text: &span.text,
                columns: span.columns,
                span,
            });
            for _ in 1..span.columns {
                cells.push(ExpandedCell {
                    text: "",
                    columns: 0,
                    span,
                });
            }
            continue;
        }
        let mut characters = span.text.char_indices();
        for _ in 0..span.columns {
            let text = characters.next().map_or("", |(start, character)| {
                &span.text[start..start + character.len_utf8()]
            });
            cells.push(ExpandedCell {
                text,
                columns: 1,
                span,
            });
        }
    }
    cells
}

fn first_cell_field_difference(
    left: &ExpandedCell<'_>,
    right: &ExpandedCell<'_>,
) -> Option<TerminalCellField> {
    let (a, b) = (left.span, right.span);
    if left.text != right.text {
        Some(TerminalCellField::Text)
    } else if left.columns != right.columns {
        Some(TerminalCellField::Columns)
    } else if a.fg != b.fg {
        Some(TerminalCellField::Fg)
    } else if a.bg != b.bg {
        Some(TerminalCellField::Bg)
    } else if a.flags != b.flags {
        Some(TerminalCellField::Flags)
    } else if a.fg_rgb != b.fg_rgb {
        Some(TerminalCellField::FgRgb)
    } else if a.bg_rgb != b.bg_rgb {
        Some(TerminalCellField::BgRgb)
    } else if a.link_uri != b.link_uri {
        Some(TerminalCellField::LinkUri)
    } else if a.link_key != b.link_key {
        Some(TerminalCellField::LinkKey)
    } else {
        None
    }
}

/// Bounded, CONTENT-FREE description: a difference is quoted into logs and
/// replay reports, so `text` and `linkUri` report only a length (in UTF-16
/// code units, as v2 measured it) and never the characters.
fn describe_cell_field(cell: &ExpandedCell<'_>, field: TerminalCellField) -> String {
    let span = cell.span;
    match field {
        TerminalCellField::Text => format!("len={}", cell.text.encode_utf16().count()),
        TerminalCellField::Columns | TerminalCellField::RowColumns => cell.columns.to_string(),
        TerminalCellField::Fg => span.fg.to_string(),
        TerminalCellField::Bg => span.bg.to_string(),
        TerminalCellField::Flags => format!("0x{:x}", span.flags),
        TerminalCellField::FgRgb => span
            .fg_rgb
            .map_or_else(|| "none".to_owned(), |rgb| format!("0x{rgb:x}")),
        TerminalCellField::BgRgb => span
            .bg_rgb
            .map_or_else(|| "none".to_owned(), |rgb| format!("0x{rgb:x}")),
        TerminalCellField::LinkUri => span.link_uri.as_ref().map_or_else(
            || "none".to_owned(),
            |uri| format!("len={}", uri.encode_utf16().count()),
        ),
        TerminalCellField::LinkKey => span.link_key.clone().unwrap_or_else(|| "none".to_owned()),
    }
}
