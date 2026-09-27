//! Pure cell-to-DOM row rendering: the xterm-palette-to-CSS mapping, one span's
//! inline style, the find-hit sub-span arithmetic, and the allocation-free row
//! hash the viewport diff compares.
//!
//! `cell_row/dom.rs` paints what this module decides; `CellGridRenderer`
//! composes the two and owns everything stateful. Nothing here reads the DOM,
//! so every rule below — including the wide-glyph box and the hash's link
//! identity folding — is testable natively.

pub mod dom;

use roost_protocol::cell::{
    CELL_BLINK, CELL_BOLD, CELL_DIM, CELL_INVISIBLE, CELL_ITALIC, CELL_REVERSE, CELL_STRIKE,
    CELL_UNDERLINE, CellRow, CellSpan, DEFAULT_COLOR, row_columns, span_is_atomic,
};

/// Class on every clickable terminal link. Anchors painted here from
/// core-authored OSC 8 cell data and anchors the linkifier wraps around
/// regex matches share it, so one CSS rule and one arm/hover/click path serve
/// both kinds.
pub const TERMINAL_LINK_CLASS: &str = "wterm-link";

/// Marks an anchor as PAINTED from cell link data and carries that link's run
/// identity. The linkifier reads it to tell painted anchors from its own and to
/// learn exactly which columns already carry a producer link. Soft-wrapped
/// halves of one link land in different rows, so this attribute re-identifies
/// them.
pub const LINK_KEY_ATTR: &str = "data-link-key";

/// Exact producer target retained separately from `href`. A file target has no
/// browser-openable href until the worker-aware resolver installs a route.
pub const TERMINAL_LINK_TARGET_ATTR: &str = "data-terminal-target";

/// The row's true GRID OCCUPANCY, stamped because nothing else can recover it
/// from the painted DOM: `textContent` length counts UTF-16 code units, and a
/// column is neither (a CJK ideograph is 2 columns / 1 unit, a ZWJ emoji
/// cluster 2 columns / 11 units). Soft-wrap grouping asks "did this row FILL
/// the grid?" — a column question.
pub const ROW_COLUMNS_ATTR: &str = "data-grid-columns";

/// Set on a row that painted at least one anchor, so a full scan can skip
/// virtually every held row with an attribute read instead of a subtree query.
/// Absent always means "no painted link on this row".
pub const ROW_HAS_LINKS_ATTR: &str = "data-row-links";

/// A find match inside one row: `len` COLUMNS starting at grid column `col`.
/// Columns are not character offsets; the worker's search RPC converts its text
/// offsets with `text_range_to_columns` before sending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FindHit {
    /// First grid column the match covers.
    pub col: u32,
    /// How many columns it covers.
    pub len: u32,
}

/// One painted piece of a span after find matches split it: a half-open column
/// interval inside the span, flagged when it is a match at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpanSlice {
    /// First column of the piece, relative to the span's own first column.
    pub start: u32,
    /// How many columns the piece covers.
    pub columns: u32,
    /// Whether this piece is a find match rather than unhighlighted text.
    pub highlighted: bool,
    /// Whether this piece is the ACTIVE match, which paints a second class.
    pub active: bool,
}

/// The six xterm 256-palette cube steps, low index first.
const CUBE_STEPS: [u16; 6] = [0, 95, 135, 175, 215, 255];

/// xterm 256-palette index to CSS. 0..15 map to the themed `--term-color-N`
/// variables; 16..231 are the 6x6x6 cube; 232..255 are the 24-step grayscale
/// ramp.
pub fn ansi256_to_css(index: u16) -> String {
    if index < 16 {
        return format!("var(--term-color-{index})");
    }
    if index >= 232 {
        let value = 8 + (index - 232) * 10;
        return format!("rgb({value},{value},{value})");
    }
    let offset = index - 16;
    let red = CUBE_STEPS[(offset / 36) as usize % 6];
    let green = CUBE_STEPS[(offset / 6) as usize % 6];
    let blue = CUBE_STEPS[(offset % 6) as usize];
    format!("rgb({red},{green},{blue})")
}

fn color_css(color: u16, rgb: Option<u32>, is_foreground: bool) -> String {
    if let Some(rgb) = rgb {
        return format!("#{:06x}", rgb & 0x00ff_ffff);
    }
    if color == DEFAULT_COLOR {
        return if is_foreground {
            "var(--term-fg)".to_string()
        } else {
            "var(--term-bg)".to_string()
        };
    }
    ansi256_to_css(color)
}

/// An atomic span is ONE cell whose glyph advance the font decides — a wide CJK
/// ideograph, an emoji, an astral codepoint. Pinning its box to its declared
/// column count is what makes a painted row exactly `cols` wide: without it a
/// 2-column emoji the font advances 1.6ch drags every later column left. A
/// coalesced narrow run needs no box; its `ch` advance is already exact.
fn occupancy_css(span: &CellSpan) -> String {
    if span_is_atomic(span) {
        format!("display:inline-block;width:{}ch", span.columns)
    } else {
        String::new()
    }
}

/// Everything `span_style` paints EXCEPT colour — including the column-occupancy
/// box, so a highlighted wide glyph keeps its width. A find-match span uses
/// this so the `.cell-find-hit` class actually owns its background and text
/// colour: an inline `color:`/`background:` beats any class rule, so emitting
/// the run's own colours on a highlighted span would leave matches looking
/// unhighlighted on styled output.
pub fn span_decoration_style(span: &CellSpan) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let box_style = occupancy_css(span);
    if !box_style.is_empty() {
        parts.push(&box_style);
    }
    let mut decorations: Vec<&str> = Vec::new();
    if span.flags & CELL_BOLD != 0 {
        parts.push("font-weight:bold");
    }
    if span.flags & CELL_DIM != 0 {
        parts.push("opacity:0.6");
    }
    if span.flags & CELL_ITALIC != 0 {
        parts.push("font-style:italic");
    }
    if span.flags & CELL_UNDERLINE != 0 {
        decorations.push("underline");
    }
    if span.flags & CELL_STRIKE != 0 {
        decorations.push("line-through");
    }
    if !decorations.is_empty() {
        parts.push("text-decoration");
        parts.extend(decorations);
    }
    if span.flags & CELL_INVISIBLE != 0 {
        parts.push("visibility:hidden");
    }
    if span.flags & CELL_BLINK != 0 {
        parts.push("animation:cell-blink 1s step-end infinite");
    }
    join_declarations(parts)
}

/// Inline CSS for one span. `reverse` swaps foreground and background;
/// `invisible` hides text. Only a non-default background is emitted, which
/// keeps the DOM lean and lets the container's `--term-bg` show through for
/// blank cells.
pub fn span_style(span: &CellSpan) -> String {
    let reverse = span.flags & CELL_REVERSE != 0;
    let foreground = color_css(span.fg, span.fg_rgb, true);
    let background = color_css(span.bg, span.bg_rgb, false);
    let (foreground, background) = if reverse {
        (background, foreground)
    } else {
        (foreground, background)
    };
    let mut parts: Vec<&str> = vec![&format!("color:{foreground}")];
    if span.bg != DEFAULT_COLOR || span.bg_rgb.is_some() || reverse {
        parts.push(&format!("background:{background}"));
    }
    let decoration = span_decoration_style(span);
    if !decoration.is_empty() {
        parts.push(&decoration);
    }
    join_declarations(parts)
}

/// Join declarations with `;`, which is what the browser reads in a `style`
/// attribute, and what a CSS text node would produce for the same rule.
fn join_declarations(parts: Vec<&str>) -> String {
    parts
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<&str>>()
        .join(";")
}

/// Split one span at every find-match boundary inside it.
///
/// An ATOMIC span is all-or-nothing, because slicing it by column would cut a
/// surrogate pair or strip a combining mark. Only a coalesced narrow run
/// splits, and there column offsets and code-unit offsets coincide by
/// construction — which is what makes the painter's text slicing exact.
///
/// A span no match touches yields one unhighlighted slice, so the caller always
/// paints the same element list it would without find state.
pub fn span_slices(span: &CellSpan, hits: &[FindHit], active_col: Option<u32>) -> Vec<SpanSlice> {
    if span_is_atomic(span) {
        let whole = SpanSlice {
            start: 0,
            columns: span.columns,
            highlighted: hits
                .iter()
                .any(|hit| overlaps(hit, 0, span.columns)),
            active: false,
        };
        let mut slices = vec![whole];
        if whole.highlighted {
            let hit = hits
                .iter()
                .find(|hit| overlaps(hit, 0, span.columns))
                .copied();
            slices[0].active = hit.is_some_and(|hit| Some(hit.col) == active_col);
        }
        return slices;
    }
    let mut slices = Vec::new();
    let mut at = 0u32;
    while at < span.columns {
        let hit = hits
            .iter()
            .find(|hit| at >= hit.col && at < hit.col.saturating_add(hit.len));
        let end = match hit {
            Some(hit) => span.columns.min(hit.col.saturating_add(hit.len)),
            None => hits
                .iter()
                .map(|hit| hit.col)
                .filter(|col| *col > at && *col < span.columns)
                .min()
                .unwrap_or(span.columns),
        };
        let end = end.max(at + 1).min(span.columns);
        slices.push(SpanSlice {
            start: at,
            columns: end - at,
            highlighted: hit.is_some(),
            active: hit.is_some_and(|hit| Some(hit.col) == active_col),
        });
        at = end;
    }
    if slices.is_empty() {
        slices.push(SpanSlice {
            start: 0,
            columns: span.columns,
            highlighted: false,
            active: false,
        });
    }
    slices
}

/// The text of one slice. A slice is only ever produced for a coalesced narrow
/// run, where every scalar is one column, so the column offset is also the
/// scalar offset.
pub fn slice_text(span: &CellSpan, slice: SpanSlice) -> String {
    let start = slice.start as usize;
    let end = start + slice.columns as usize;
    span.text
        .chars()
        .skip(start)
        .take(slice.columns as usize)
        .collect()
}

fn overlaps(hit: &FindHit, start: u32, columns: u32) -> bool {
    let hit_end = hit.col.saturating_add(hit.len);
    let slice_end = start.saturating_add(columns);
    start < hit_end && hit.col < slice_end
}

const FNV_OFFSET: u32 = 2_166_136_261;
const FNV_PRIME: u32 = 16_777_619;

fn fold(hash: u32, value: u32) -> u32 {
    hash.wrapping_mul(FNV_PRIME) ^ value
}

/// Visual identity of a row, as a 32-bit FNV-1a hash of every attribute the row
/// painter sets: span structure, text, colors, flags, link identity and find
/// hits. Rows with equal hashes paint identically, so the viewport diff skips
/// them.
///
/// Allocation-free on purpose — a string-keyed hash would build one string per
/// viewport row per frame AND re-derive every span's style for every row that
/// changed. Span count, per-span text length and per-span column occupancy are
/// folded in so a different span split, or the same text at a different width,
/// cannot collide.
///
/// LINK identity is folded in as the run key plus the URI's LENGTH, not the URI
/// itself: the key is present exactly when the URI is, and inside one grid
/// epoch it maps one-to-one onto a URI, because a core rebuild arrives as a NEW
/// epoch that repaints every row unconditionally. Hashing a 2 KB URI per span
/// per row per frame would cost far more than the collision it rules out.
pub fn row_hash(row: &CellRow, hits: Option<&[FindHit]>, active_col: Option<u32>) -> u32 {
    let mut hash =
        (FNV_OFFSET ^ u32::try_from(row.spans.len()).unwrap_or(u32::MAX)).wrapping_mul(FNV_PRIME);
    for span in row.spans.iter() {
        let units: Vec<u16> = span.text.encode_utf16().collect();
        hash = fold(hash, u32::try_from(units.len()).unwrap_or(u32::MAX));
        hash = fold(hash, span.columns);
        for unit in units {
            hash = fold(hash, u32::from(unit));
        }
        hash = fold(hash, span.fg.into());
        hash = fold(hash, span.bg.into());
        hash = fold(hash, span.flags.into());
        hash = fold(hash, span.fg_rgb.map_or(u32::MAX, |rgb| rgb));
        hash = fold(hash, span.bg_rgb.map_or(u32::MAX, |rgb| rgb));
        match span.link_key.as_deref() {
            None => hash = fold(hash, 0),
            Some(key) => {
                hash = fold(hash, u32::try_from(key.encode_utf16().count()).unwrap_or(u32::MAX) + 1);
                for unit in key.encode_utf16() {
                    hash = fold(hash, u32::from(unit));
                }
                hash = fold(
                    hash,
                    span.link_uri
                        .as_deref()
                        .map_or(0, |uri| u32::try_from(uri.encode_utf16().count()).unwrap_or(u32::MAX)),
                );
            }
        }
    }
    if let Some(hits) = hits {
        for hit in hits {
            hash = fold(hash, hit.col);
            hash = fold(hash, hit.len);
        }
        hash = fold(hash, active_col.unwrap_or(u32::MAX));
    }
    hash
}

/// The grid occupancy the row painter stamps on the element, so the row's
/// column count is recoverable from the DOM alone.
pub fn row_column_count(row: &CellRow) -> u32 {
    row_columns(&row.spans)
}
