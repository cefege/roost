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
//! Ports v2 `apps/worker/src/browser-commands/browser-command-terminal.ts`.

use std::sync::Arc;

use roost_protocol::cell::{CellRow, CellSpan};
use roost_protocol::wire::brand::SessionId;
use roost_term::frame::{scrollback_offset_spans, viewport_row_spans};
use roost_term::{read_scrollback_range, scrollback_origin};
use serde::Serialize;
use serde::ser::{SerializeStruct, Serializer};

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

    fn row(&self, session_id: SessionId, absolute_row: u32) -> Boxed<Option<CellRowJson>> {
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
        viewport_rows: core.rows(),
    })
}

/// What one session's grid looks like, for a reader outside this file.
///
/// `pub(crate)` rather than `pub` because the only other reader is the
/// scrollback SCANNER, and it needs the same four numbers a page request gets.
/// A scanner that derived the floor itself would be a second answer to "where
/// does this grid start", which is the question the epoch exists to keep
/// singular — so it asks here.
pub(crate) fn describe_grid(record: &SessionRecord) -> Result<GridDescription, Refusal> {
    describe(record)
}

/// One row's painted spans, by its absolute index, or `None` when the grid no
/// longer holds it.
///
/// The scanner needs SPANS, not the JSON projection [`row_value`] produces: a
/// match is a scalar offset into a row's text that has to be converted back to
/// grid columns, and the JSON has already thrown that mapping away.
pub(crate) fn row_spans(record: &SessionRecord, absolute_row: u32) -> Option<Arc<[CellSpan]>> {
    let core = record.terminal_core.as_ref();
    let floor = scrollback_origin(core, record.cell_emit.scrollback_origin).ok()?;
    let index = u64::from(absolute_row);
    if index < floor {
        return None;
    }
    let retained = core.scrollback_count() as u64;
    let scrollback_total = floor.saturating_add(retained);
    if index < scrollback_total {
        // Oldest-first absolute index to the core's newest-first offset.
        let offset = retained - 1 - (index - floor);
        return Some(scrollback_offset_spans(core, usize::try_from(offset).ok()?));
    }
    let viewport_row = index - scrollback_total;
    if viewport_row >= u64::from(core.rows()) {
        return None;
    }
    let row = u16::try_from(viewport_row).ok()?;
    Some(viewport_row_spans(core, row, core.cols()))
}

/// One row by its absolute index as the value model, or `None` when the grid
/// no longer holds it. The page reader and the direct scrollback reader
/// (`local_terminal::scrollback`) both read rows through here.
///
/// `read_scrollback_range` CLAMPS to the retained window, which is the whole
/// answer here: a row below the floor or at the end reads as an empty range, and
/// an empty range is a row this grid does not have.
pub(crate) fn row_cells(record: &SessionRecord, absolute_row: u32) -> Option<CellRow> {
    let core = record.terminal_core.as_ref();
    let origin = scrollback_origin(core, record.cell_emit.scrollback_origin).ok()?;
    let index = u64::from(absolute_row);
    read_scrollback_range(core, index, index + 1, origin)
        .into_iter()
        .next()
}

fn row_value(record: &SessionRecord, absolute_row: u32) -> Option<CellRowJson> {
    // OWNED, not borrowed, and the lifetime says so. The row is built inside a
    // closure over the session table, and nothing may borrow out of that; what
    // it costs is one `Arc` bump, because `CellRow`'s spans are already behind
    // one. Everything expensive — the span text, the link URI — stays behind
    // the borrow that `Serialize` takes below.
    row_cells(record, absolute_row).map(CellRowJson::owned)
}

/// A monotonic history index as the `u32` the page arithmetic speaks.
///
/// Saturating rather than refused: the wire's row indices are `u32`, so a grid
/// past four billion rows is a grid no client can address, and clamping keeps
/// every index it CAN address pointing at the row it named.
fn narrow(index: u64) -> u32 {
    u32::try_from(index).unwrap_or(u32::MAX)
}

/// A cell row that serialises itself in the order the value model declares.
///
/// `pub` because `browser_commands::scrollback_page::RetainedGrid` returns it
/// across a crate boundary, and a private type in a public signature is a type
/// no caller can name.
///
/// IT OWNS THE ROW, AND THAT IS NOT A LOSS. This was a `Cow<'a, CellRow>` with a
/// borrowed arm, and the borrowed arm was unreachable: its only constructor was
/// `cell_row_json`, and `cell_row_json` had no caller in `src/` — only its own
/// test. A `Cow` variant nothing constructs is the same defect one level below
/// the one it was added to fix, so both are gone rather than kept on the
/// strength of being documented and tested. The cost is one `Arc` bump, because
/// `CellRow`'s spans are already behind an `Arc`; the benefit is a type with
/// one representation instead of two, one of which was fiction.
///
/// ONE PRODUCER, AND A SECOND ONE COSTS A WIRE PIN. `RetainedGrid::row` is the
/// only thing in the crate that builds this, and `tests/cell_row_json.rs` is
/// the only test of how it SPELLS itself — so a second representation added
/// later would ship with nothing comparing the two, and a field order that
/// quietly diverges is a browser painting a colour the worker never observed.
/// That is why the test builds through `owned` rather than around it: it pins
/// the production path. If you add a variant, it needs a case in that test
/// before it needs anything else.
#[derive(Debug, Clone)]
pub struct CellRowJson(CellRow);

impl CellRowJson {
    /// The projection over a row the caller already holds.
    ///
    /// For `RetainedGrid::row`, whose row is built inside a closure over the
    /// session table and cannot be borrowed out of.
    pub fn owned(row: CellRow) -> Self {
        Self(row)
    }
}

impl Serialize for CellRowJson {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut row = serializer.serialize_struct("CellRow", 3)?;
        row.serialize_field("index", &self.0.index)?;
        row.serialize_field("spans", &CellSpansJson(&self.0.spans))?;
        row.serialize_field("mark", &self.0.mark)?;
        row.end()
    }
}

/// The spans of one row, as a sequence of borrowed spans.
struct CellSpansJson<'a>(&'a [CellSpan]);

impl Serialize for CellSpansJson<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.0.iter().map(CellSpanJson))
    }
}

/// One span, in the value model's declaration order.
///
/// The four scalars are ALWAYS written and the four optionals only when the
/// core authored them, so the length handed to `serialize_struct` counts only
/// the ones that will follow: a serializer told the wrong length is being lied
/// to about its own output.
struct CellSpanJson<'a>(&'a CellSpan);

impl Serialize for CellSpanJson<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let span = self.0;
        let optionals = usize::from(span.fg_rgb.is_some())
            + usize::from(span.bg_rgb.is_some())
            + usize::from(span.link_uri.is_some())
            + usize::from(span.link_key.is_some());
        let mut fields = serializer.serialize_struct("CellSpan", 4 + optionals)?;
        fields.serialize_field("text", &span.text)?;
        fields.serialize_field("fg", &span.fg)?;
        fields.serialize_field("bg", &span.bg)?;
        fields.serialize_field("flags", &span.flags)?;
        if let Some(true_colour) = span.fg_rgb {
            fields.serialize_field("fgRgb", &true_colour)?;
        }
        if let Some(true_colour) = span.bg_rgb {
            fields.serialize_field("bgRgb", &true_colour)?;
        }
        fields.serialize_field("columns", &span.columns)?;
        if let Some(uri) = span.link_uri.as_deref() {
            fields.serialize_field("linkUri", &uri)?;
        }
        if let Some(key) = span.link_key.as_deref() {
            fields.serialize_field("linkKey", &key)?;
        }
        fields.end()
    }
}
