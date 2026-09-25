//! The cell-grid value model — a span, a row, one frame — and the single
//! implementation of "which grid column is this text at".
//!
//! The worker owns the one emulator and ships PRE-RENDERED styled cells; every
//! front end paints them and never re-parses VT or reflows. `diff_grid` and
//! `delta_batch` fold frames of these types, `proto` puts them on the wire, and
//! the find path turns text offsets into columns through the functions below.
//! A second geometry implementation is how a wide glyph gets painted one
//! column short, so nothing may re-derive columns from the text.

use std::sync::Arc;

use crate::error::{ProtocolError, ProtocolResult};

/// Palette value meaning "the terminal's own default colour", which is neither
/// an ANSI index nor a palette entry.
pub const DEFAULT_COLOR: u16 = 256;

// Style flag bits — identical layout to the terminal core's cell flags. The
// renderer maps these to CSS, so a changed bit value changes what a browser
// paints.
pub const CELL_BOLD: u16 = 0x01;
pub const CELL_DIM: u16 = 0x02;
pub const CELL_ITALIC: u16 = 0x04;
pub const CELL_UNDERLINE: u16 = 0x08;
pub const CELL_BLINK: u16 = 0x10;
pub const CELL_REVERSE: u16 = 0x20;
pub const CELL_INVISIBLE: u16 = 0x40;
pub const CELL_STRIKE: u16 = 0x80;

/// Wire bound on one span's link URI, in UTF-8 BYTES. An over-cap URI loses the
/// LINK and keeps the TEXT: a truncated URI retargets the click elsewhere.
pub const MAX_LINK_URI_BYTES: usize = 2048;

// COLUMN OCCUPANCY. A span is either a coalesced narrow RUN — every cell one
// column and one scalar, so grid column i holds text's i-th character — or an
// ATOMIC cell that must never be sliced: a width-2 LEAD with its width-0
// CONTINUATION folded in, an astral codepoint, or a grapheme cluster. A run
// holds only one-scalar cells, so `columns` can only be read off the text when
// the span is one of those runs. Every offset below is a UNICODE SCALAR offset:
// not a UTF-16 code unit, because a `str` slices by byte and a mid-character
// byte slice panics, and not a byte, because a multi-byte glyph is one cell.

/// A run of consecutive cells sharing one style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellSpan {
    /// Concatenated codepoints of the run (NUL cells render as a space).
    pub text: String,
    /// 0..15 ANSI, 16..255 palette, `DEFAULT_COLOR` = default. As `fg` for `bg`.
    pub fg: u16,
    pub bg: u16,
    /// `CELL_*` bitfield, as the core reports it.
    pub flags: u16,
    /// Resolved 24-bit fg (0xRRGGBB) when the core provides true color.
    pub fg_rgb: Option<u32>,
    pub bg_rgb: Option<u32>,
    /// REQUIRED terminal columns this span occupies (>= 1). Equal to the scalar
    /// count only for a coalesced narrow run; see the note above.
    pub columns: u32,
    /// Core-authored OSC 8 URI for every cell of this span, or absent. Roost's
    /// ONLY hyperlink source: nothing re-derives links from bytes or matches
    /// link text.
    pub link_uri: Option<String>,
    /// Opaque per-core RUN identity for that link, present exactly when
    /// `link_uri` is: two OSC 8 emissions can share one URI and must stay two
    /// independently clickable spans, so this is the coalescing key. Never
    /// compare it across a rebuild.
    pub link_key: Option<String>,
}

/// One painted row: a viewport row is numbered 0..rows-1 within the current
/// grid, a scrollback row by absolute line, 0 = oldest retained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellRow {
    /// Row coordinate, as described on the type.
    pub index: u32,
    /// Immutable after decoding. Shared rather than copied: a replica clone
    /// renumbers rows, and cells a renderer already painted must not be copied
    /// to do that.
    pub spans: Arc<[CellSpan]>,
}

/// A column range of a row, the output of `text_range_to_columns`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnRange {
    /// First column covered.
    pub col: u32,
    /// Columns covered; never zero for a non-empty text range.
    pub columns: u32,
}

/// Authoritative checkpoints are viewport-only; pure full-frame callers pass
/// this explicit zero-tail value instead of a second policy.
pub const SB_SNAPSHOT_HISTORY_ROWS: usize = 0;

/// Mouse reporting mode the foreground application requested: 0 = none, 1000 =
/// press/release, 1002 = press/release plus motion while held. The core folds
/// legacy mode 9 and any-motion 1003 into these three, so no other value is
/// representable — but the wire is a raw integer, so one still has to decode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MouseTracking {
    /// No tracking requested, so the browser keeps native selection and scroll.
    #[default]
    None,
    /// DECSET 1000: press and release.
    PressRelease,
    /// DECSET 1002: press and release, plus motion while a button is held.
    ButtonMotion,
    /// A mode this build does not name, carried verbatim so it round-trips
    /// instead of failing to decode.
    Unknown(u32),
}

impl From<MouseTracking> for u32 {
    fn from(mode: MouseTracking) -> Self {
        match mode {
            MouseTracking::None => 0,
            MouseTracking::PressRelease => 1000,
            MouseTracking::ButtonMotion => 1002,
            MouseTracking::Unknown(raw) => raw,
        }
    }
}

impl From<u32> for MouseTracking {
    fn from(raw: u32) -> Self {
        match raw {
            0 => Self::None,
            1000 => Self::PressRelease,
            1002 => Self::ButtonMotion,
            raw => Self::Unknown(raw),
        }
    }
}

/// Narrow a wire integer to a MouseTracking. An unknown value reads as "no
/// tracking requested", the safe answer for a mode neither side agreed on.
pub fn as_mouse_tracking(raw: u32) -> MouseTracking {
    match MouseTracking::from(raw) {
        MouseTracking::Unknown(_) => MouseTracking::None,
        mode => mode,
    }
}

/// One terminal grid as the worker renders it; `full` marks a viewport-only
/// authoritative checkpoint, as opposed to a delta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellGridFrame {
    /// Coordinator-minted generation for one watched-session stream.
    pub stream_id: String,
    /// Opaque identity for the worker-side grid numbering epoch.
    pub grid_epoch: String,
    pub cols: u32,
    pub rows: u32,
    pub cursor_row: u32,
    pub cursor_col: u32,
    pub cursor_visible: bool,
    pub alt_screen: bool,
    /// DECCKM app-cursor-keys mode; drives the front end's keystroke encoder.
    pub cursor_keys_app: bool,
    /// DECSET 2004 bracketed-paste mode; drives paste wrapping.
    pub bracketed_paste: bool,
    /// Mouse reporting the FOREGROUND APPLICATION requested (DECSET 1000/1002),
    /// read off the core and never inferred from alt-screen occupancy.
    pub mouse_tracking: MouseTracking,
    /// DECSET 1006; picks the mouse-report encoding (SGR-1006 vs X10).
    pub mouse_sgr: bool,
    /// DECSET 1004; the front end reports real textarea focus/blur as CSI I / O.
    pub focus_events: bool,
    /// true = full snapshot (every viewport row); false = delta (changed rows
    /// only, plus the newly pushed lines in scrollback_append).
    pub full: bool,
    pub viewport_rows: Vec<CellRow>,
    /// History carried by a full frame before canonical normalization.
    pub scrollback_rows: Vec<CellRow>,
    /// Delta frames only: lines appended since the prior frame, oldest →
    /// newest, each carrying its absolute line number.
    pub scrollback_append: Vec<CellRow>,
    /// Total retained scrollback line count at frame time.
    pub scrollback_total: u64,
    /// Absolute index of scrollback_rows[0]; 0 = complete from the oldest
    /// retained line. A delta always carries 0: appends index themselves.
    pub sb_base: u64,
    /// Previous emitted sequence for a delta; full baselines always carry 0.
    pub base_seq: u64,
    /// Monotonic sequence within the stream; gaps require a new full baseline.
    pub seq: u64,
}

/// True when `uri` fits MAX_LINK_URI_BYTES once UTF-8 encoded. Exact and
/// allocation-free: this runs on the per-cell encoding path, so encoding every
/// URI to measure it is not affordable.
pub fn link_uri_within_cap(uri: &str) -> bool {
    if uri.len() > MAX_LINK_URI_BYTES {
        return false;
    }
    if uri.len() * 3 <= MAX_LINK_URI_BYTES {
        return true;
    }
    let mut bytes = 0usize;
    for character in uri.chars() {
        bytes += character.len_utf8();
        if bytes > MAX_LINK_URI_BYTES {
            return false;
        }
    }
    true
}

/// True when the span is ONE grid cell that must be painted and highlighted
/// whole: a wide lead, an astral codepoint, or a grapheme cluster. A run only
/// ever holds one-scalar narrow cells, so an atomic span is one whose column
/// count disagrees with its scalar count — OR whose text holds a scalar above
/// the BMP, which is what a code-unit count saw as a surrogate. "🐙" in one
/// column is that second case alone: one scalar, one column, one glyph.
pub fn span_is_atomic(span: &CellSpan) -> bool {
    if span.columns as usize != scalar_count(span) {
        return true;
    }
    span.text.chars().any(|character| character > '\u{ffff}')
}

/// Terminal columns the spans occupy: less than the grid width for a
/// right-trimmed row, never more.
pub fn row_columns(spans: &[CellSpan]) -> u32 {
    let columns: u64 = spans.iter().map(|span| u64::from(span.columns)).sum();
    u32::try_from(columns).unwrap_or(u32::MAX)
}

/// Row text as painted: continuation columns contribute nothing, so "中文" is
/// two characters over four columns.
pub fn spans_text(spans: &[CellSpan]) -> String {
    let mut text = String::new();
    for span in spans {
        text.push_str(&span.text);
    }
    text
}

/// Grid column of a scalar `offset`; an atomic span resolves to its first column.
pub fn text_offset_to_column(spans: &[CellSpan], offset: usize) -> u32 {
    columns_at_offset(spans, offset).0
}

/// Exclusive-end companion: one past the last column a range ending at scalar
/// `offset` covers, with an atomic span covered whole.
pub fn text_offset_to_column_end(spans: &[CellSpan], offset: usize) -> u32 {
    columns_at_offset(spans, offset).1
}

/// The start and exclusive-end columns of one scalar offset, from one walk. An
/// offset between two spans covers neither and both answers are the accumulated
/// column; an offset inside an atomic span starts at its first column and ends
/// past all of them, which is what keeps a match off half a grapheme.
fn columns_at_offset(spans: &[CellSpan], offset: usize) -> (u32, u32) {
    let mut at = 0usize;
    let mut col = 0u32;
    for span in spans {
        if offset == at {
            return (col, col);
        }
        let next = at + scalar_count(span);
        if offset < next {
            return if span_is_atomic(span) {
                (col, col + span.columns)
            } else {
                let inside = col + (offset - at) as u32;
                (inside, inside)
            };
        }
        at = next;
        col = col.saturating_add(span.columns);
    }
    (col, col)
}

/// Column range of a `[offset, offset + length)` scalar range: the conversion
/// every match and highlight crosses.
pub fn text_range_to_columns(spans: &[CellSpan], offset: usize, length: usize) -> ColumnRange {
    let col = text_offset_to_column(spans, offset);
    if length == 0 {
        return ColumnRange { col, columns: 0 };
    }
    let end = text_offset_to_column_end(spans, offset + length);
    ColumnRange {
        col,
        columns: end.saturating_sub(col).max(1),
    }
}

/// The span painted at grid column `col` with the column's offset inside it.
pub fn column_span(spans: &[CellSpan], col: i64) -> Option<(&CellSpan, u32)> {
    if col < 0 {
        return None;
    }
    let mut at = 0i64;
    for span in spans {
        let next = at + i64::from(span.columns);
        if col < next {
            return Some((span, (col - at) as u32));
        }
        at = next;
    }
    None
}

/// Text painted at grid column `col`: the whole glyph for an atomic span, one
/// character of a run, `""` past the row's end.
pub fn column_text(spans: &[CellSpan], col: i64) -> &str {
    let Some((span, offset)) = column_span(spans, col) else {
        return "";
    };
    if span_is_atomic(span) {
        return &span.text;
    }
    match span.text.char_indices().nth(offset as usize) {
        Some((start, character)) => &span.text[start..start + character.len_utf8()],
        None => "",
    }
}

/// Decode-side contract check: every span claims at least one column and
/// carries text, and a viewport row claims no more columns than the grid has.
/// `max_columns == 0` skips the width bound, because a retained scrollback line
/// keeps its write-time width.
///
/// Link identity is checked here too, because a span is what gets CLICKED: a URI
/// over the cap, an empty one, or a key with no URI would each reach the user as
/// a broken or unbounded link rather than as a decode error. The rules run in
/// this order, so a row with two faults always reports the same one.
pub fn assert_cell_row_spans(row: &CellRow, max_columns: u32) -> ProtocolResult<()> {
    let path = format!("cell_row[{}]", row.index);
    let mut columns = 0u64;
    for (position, span) in row.spans.iter().enumerate() {
        let span_path = format!("{path}.spans[{position}]");
        if span.columns < 1 {
            return Err(ProtocolError::new(
                format!("{span_path}.columns"),
                format!(
                    "span {} claims {} columns",
                    serde_json::Value::String(span.text.clone()),
                    span.columns
                ),
            ));
        }
        if span.text.is_empty() {
            return Err(ProtocolError::new(
                format!("{span_path}.text"),
                format!("span at column {columns} carries no text"),
            ));
        }
        // A URI without a run key cannot be grouped by a renderer, and a key
        // without a URI links nowhere. An empty key is a missing key.
        if let Some(uri) = &span.link_uri {
            if uri.is_empty() {
                return Err(ProtocolError::new(
                    format!("{span_path}.link_uri"),
                    format!("span at column {columns} carries an empty link_uri"),
                ));
            }
            if !link_uri_within_cap(uri) {
                return Err(ProtocolError::new(
                    format!("{span_path}.link_uri"),
                    format!(
                        "span at column {columns} carries a link_uri over \
                         {MAX_LINK_URI_BYTES} bytes"
                    ),
                ));
            }
            if !span.link_key.as_ref().is_some_and(|key| !key.is_empty()) {
                return Err(ProtocolError::new(
                    format!("{span_path}.link_key"),
                    format!("span at column {columns} carries link_uri with no link_key"),
                ));
            }
        } else if span.link_key.is_some() {
            return Err(ProtocolError::new(
                format!("{span_path}.link_key"),
                format!("span at column {columns} carries link_key with no link_uri"),
            ));
        }
        columns += u64::from(span.columns);
    }
    if max_columns > 0 && columns > u64::from(max_columns) {
        return Err(ProtocolError::new(
            format!("{path}.spans"),
            format!("spans occupy {columns} columns of a {max_columns}-column grid"),
        ));
    }
    Ok(())
}

/// The scalar count a span's offsets are counted in. Recomputed per span in each
/// walk above, because precomputing it would allocate on the per-cell path.
fn scalar_count(span: &CellSpan) -> usize {
    span.text.chars().count()
}
