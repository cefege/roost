//! A live session's grid, as a browser-command page reads it. This is the
//! PRODUCTION `impl browser_commands::scrollback_page::RetainedGrid`; the
//! dispatch's `Deps::grid` is one of these over the worker's `SessionTable`.
//! Depends on `super::lifecycle` for the table, `crate::scrollback_read` for the
//! page arithmetic it hands rows to, and `roost_term` for the row addressing —
//! and on nothing that depends on it back.
//!
//! THE PAGE ARITHMETIC IS NOT HERE. `crate::scrollback_read` is the one reader:
//! the bounds, the clamping, the epoch fence and the slice walk all live there,
//! and the local door's scrollback socket wraps the same thing. This file
//! answers two questions it asks — what does this grid look like right now, and
//! what is this one row — and nothing else.
//!
//! THE EPOCH IS READ FROM THE CORE'S OWN EMIT STATE, NEVER FROM THE LAST
//! EMITTED FRAME. The ring keeps evicting between emits, so a stale origin
//! shifts every offset the absolute indices resolve through: real rows under the
//! wrong indices, which is worse than a short page because the client cannot
//! tell.
//!
//! BOTH METHODS READ SYNCHRONOUSLY AND RETURN OWNED VALUES. The `Boxed` future
//! a capability trait returns is a shape, not a requirement to do I/O in it, and
//! a guard held across it would make this type's `Send`ness a function of how
//! the session table happens to be locked.

use std::sync::Arc;

use roost_protocol::cell::{CellRow, CellSpan};
use roost_protocol::wire::brand::SessionId;
use roost_term::{read_scrollback_range, scrollback_origin};
use serde_json::{Map, Value, json};

use super::lifecycle::SessionTable;
use super::types::SessionRecord;
use crate::browser_commands::scrollback_page::{GridDescription, RetainedGrid};
use crate::browser_commands::{Boxed, Refusal};
use crate::scrollback_read::EpochBinding;

/// The command this capability answers, and the one a refusal is attributed to.
const COMMAND: &str = "get-scrollback-cells";

/// A worker's live sessions, read as a retained grid.
#[derive(Debug)]
pub struct SessionGrid {
    table: Arc<SessionTable>,
}

impl SessionGrid {
    /// A grid over every session this worker holds.
    pub fn new(table: Arc<SessionTable>) -> Self {
        Self { table }
    }

    /// Take one reading of a session's grid, or the refusal for a session this
    /// worker does not hold.
    fn describe_one(&self, session_id: &SessionId) -> Result<GridDescription, Refusal> {
        self.table
            .with_record(session_id, describe)
            .unwrap_or_else(|| Err(Refusal::failed(COMMAND, "unknown session")))
    }
}

impl RetainedGrid for SessionGrid {
    fn describe(&self, session_id: SessionId) -> Boxed<Result<GridDescription, Refusal>> {
        let described = self.describe_one(&session_id);
        Box::pin(async move { described })
    }

    fn row(&self, session_id: SessionId, absolute_row: u32) -> Boxed<Option<Value>> {
        let read = self
            .table
            .with_record(&session_id, |record| row_value(record, absolute_row))
            .flatten();
        Box::pin(async move { read })
    }
}

/// What one session's grid looks like, taken once.
///
/// A core that cannot report how many lines its own ring has discarded is
/// REFUSED, not approximated: the frame that would have carried a wrong index
/// is worse than no frame, because the caller cannot tell the two apart.
fn describe(record: &SessionRecord) -> Result<GridDescription, Refusal> {
    let core = record.terminal_core.as_ref();
    let origin = scrollback_origin(core, record.cell_emit.scrollback_origin)
        .map_err(|error| Refusal::failed(COMMAND, error.to_string()))?;
    let retained = core.scrollback_count() as u64;
    Ok(GridDescription {
        binding: EpochBinding::new(record.cell_emit.grid_epoch()),
        retained_floor: narrow(origin),
        // The pin's replay floor is the highest a REPLAY BOUND has ever
        // established, so it is the one number that says whether the rows below
        // the current floor are gone forever or only lost to a rebuild.
        resize_replay_floor: record
            .sb_origin_pin
            .map_or(0, |pin| narrow(pin.replay_floor)),
        total: narrow(origin.saturating_add(retained)),
        cols: core.cols(),
    })
}

/// One row by its absolute index, or `None` when the grid no longer holds it.
///
/// `read_scrollback_range` CLAMPS to the retained window, which is the whole
/// answer here: a row below the floor or at the end reads as an empty range, and
/// an empty range is a row this grid does not have.
fn row_value(record: &SessionRecord, absolute_row: u32) -> Option<Value> {
    let core = record.terminal_core.as_ref();
    let origin = scrollback_origin(core, record.cell_emit.scrollback_origin).ok()?;
    let index = u64::from(absolute_row);
    let rows = read_scrollback_range(core, index, index + 1, origin);
    rows.first().map(cell_row_json)
}

/// A monotonic history index as the `u32` the page arithmetic speaks.
///
/// Saturating rather than refused: the wire's row indices are `u32`, so a grid
/// past four billion rows is a grid no client can address, and clamping keeps
/// every index it CAN address pointing at the row it named.
fn narrow(index: u64) -> u32 {
    u32::try_from(index).unwrap_or(u32::MAX)
}

/// One cell row as the JSON a browser already parses.
///
/// The shape is the cell value model's, NOT proto3 JSON. `PbCellRow`'s own
/// serde form omits a zero scalar — `"fg":0` disappears — and the renderer
/// compares spans by identity and looks a palette entry up by that same field,
/// so an omitted `fg` is a black span that compares unequal to the identical
/// black span beside it. Every field the browser reads is therefore always
/// present, and the optional fields appear exactly when the core authored
/// them, in the order the value model declares them.
/// `roost_protocol::cell::proto` is where this belongs beside the row's other
/// projection; it is here because no JSON spelling of a cell row exists there
/// yet, and the browser's is the one this command must produce.
pub fn cell_row_json(row: &CellRow) -> Value {
    let spans: Vec<Value> = row.spans.iter().map(cell_span_json).collect();
    json!({ "index": row.index, "spans": spans })
}

fn cell_span_json(span: &CellSpan) -> Value {
    let mut object = Map::with_capacity(9);
    object.insert("text".to_owned(), Value::from(span.text.clone()));
    object.insert("fg".to_owned(), Value::from(span.fg));
    object.insert("bg".to_owned(), Value::from(span.bg));
    object.insert("flags".to_owned(), Value::from(span.flags));
    if let Some(red_green_blue) = span.fg_rgb {
        object.insert("fgRgb".to_owned(), Value::from(red_green_blue));
    }
    if let Some(red_green_blue) = span.bg_rgb {
        object.insert("bgRgb".to_owned(), Value::from(red_green_blue));
    }
    object.insert("columns".to_owned(), Value::from(span.columns));
    if let Some(uri) = span.link_uri.as_deref() {
        object.insert("linkUri".to_owned(), Value::from(uri));
    }
    if let Some(key) = span.link_key.as_deref() {
        object.insert("linkKey".to_owned(), Value::from(key));
    }
    Value::Object(object)
}
