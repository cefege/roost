//! One row of cells into right-trimmed, run-length-encoded spans.
//!
//! This is the file the v2 terminal was written around: it is the inverse of
//! the browser's paint, and its two rules are load-bearing.
//!
//! **A wide glyph's continuation column is folded into its lead.** Emitting it
//! as a cell of its own paints "中  文" for a grid holding "中文" and shifts
//! every column to its right. An orphan continuation — one whose lead was
//! overwritten by a narrow character, which leaves the tail behind on some
//! cores — becomes one blank column so the row still spans exactly its cells.
//!
//! **A run is broken by link identity, not by style.** Two adjacent runs can
//! share a style, even a URI, and still be two separately clickable links.
//! Merging them would ship one span carrying one link and lose the other
//! silently.

use roost_protocol::cell::{CellSpan, DEFAULT_COLOR, MAX_LINK_URI_BYTES};

use crate::core::CellData;

/// Whether a URI fits the wire cap.
///
/// Allocation-free because it runs per cell on the encoding path: the fast
/// answer is arithmetic on the byte length, and only an ambiguous length walks
/// the string. `len * 3 <= cap` is sound because a UTF-8 sequence is at most 3
/// bytes per UTF-16 code unit, surrogate pairs included.
pub fn link_uri_within_cap(uri: &str) -> bool {
    let len = uri.len();
    if len > MAX_LINK_URI_BYTES {
        return false;
    }
    if len * 3 <= MAX_LINK_URI_BYTES {
        return true;
    }
    uri.len() <= MAX_LINK_URI_BYTES
}

/// Trim a trailing run of default-style blanks.
///
/// A width-0 cell backed by a wide lead is that glyph's SECOND column, not
/// padding: trimming it would shrink the lead's occupancy and un-align the tail
/// of the row.
fn trimmed_end(cells: &[CellData], length: usize) -> usize {
    let mut end = length;
    while end > 0 {
        let cell = &cells[end - 1];
        if !is_blank_default(cell) {
            break;
        }
        if cell.width == 0 {
            let mut lead = end as i64 - 2;
            while lead >= 0 && cells[lead as usize].width == 0 {
                lead -= 1;
            }
            if lead >= 0 && cells[lead as usize].width >= 2 {
                break;
            }
        }
        end -= 1;
    }
    end
}

/// Trimmable padding. A LINKED cell never qualifies however blank it looks:
/// its columns are clickable, so trimming them would shrink the hyperlink.
fn is_blank_default(cell: &CellData) -> bool {
    cell.link_uri.is_none()
        && cell.link_key.is_none()
        && (cell.character == ' ' as u32 || cell.character == 0)
        && cell.fg == DEFAULT_COLOR
        && cell.bg == DEFAULT_COLOR
        && cell.flags == 0
        && cell.combining.is_none()
}

/// Can `cell` extend the open run `run`?
///
/// Style equality is not enough, and neither is URI equality: the link KEY is
/// what distinguishes two runs of the same destination.
fn same_run(cell: &CellData, run: &CellSpan, link_key: Option<&str>) -> bool {
    cell.fg == run.fg
        && cell.bg == run.bg
        && cell.flags == run.flags
        && cell.fg_rgb == run.fg_rgb
        && cell.bg_rgb == run.bg_rgb
        && run.link_key.as_deref() == link_key
        && run.link_uri.as_deref()
            == if link_key.is_none() {
                None
            } else {
                cell.link_uri.as_deref()
            }
}

/// The link a cell contributes, or `None` when it has none, when the core gave
/// a URI with no run key, or when the URI blew the wire cap — the last of which
/// drops the LINK and keeps the text, because a truncated URI would point
/// somewhere else entirely.
fn link_of(cell: &CellData) -> (Option<String>, Option<String>) {
    let Some(uri) = cell.link_uri.as_deref() else {
        return (None, None);
    };
    let Some(key) = cell.link_key.as_deref() else {
        return (None, None);
    };
    if !link_uri_within_cap(uri) {
        return (None, None);
    }
    (Some(uri.to_owned()), Some(key.to_owned()))
}

/// Run-length encode one row. `length` bounds a borrowed row, so trailing
/// scratch cells never enter a canonical span. An empty row encodes to `[]`.
pub fn row_to_spans(cells: &[CellData], length: usize) -> Vec<CellSpan> {
    let end = trimmed_end(cells, length);
    let mut spans: Vec<CellSpan> = Vec::new();
    // The open run, tracked as an index rather than read back off `spans`.
    // An atomic cell clears it, which is the whole reason it is a separate
    // value: a wide glyph's span must never be extended by the narrow cell
    // after it, because its `columns` is that glyph's own occupancy and a
    // merge would claim a column it does not own.
    let mut open: Option<usize> = None;
    let mut col = 0usize;

    while col < end {
        let cell = &cells[col];
        let (link_uri, link_key) = link_of(cell);

        // A cell of exactly one column and one scalar value coalesces, so a run
        // keeps column i == text[i]. An astral codepoint, a cluster, a lone
        // surrogate, or anything wide is one cell of several and stays atomic.
        // `char::from_u32` is what rules out the surrogate case: a `char` cannot
        // hold one, and a core that reported one must not crash the encoder.
        let scalar = char::from_u32(cell.character).filter(|c| (*c as u32) <= 0xFFFF);
        let has_cluster = cell
            .combining
            .as_ref()
            .is_some_and(|combining| !combining.is_empty());
        if cell.width == 1
            && !has_cluster
            && let Some(character) = scalar
        {
            let extends = open
                .and_then(|index| spans.get(index))
                .is_some_and(|run| same_run(cell, run, link_key.as_deref()));
            if extends && let Some(run) = open.and_then(|index| spans.get_mut(index)) {
                run.text.push(character);
                run.columns += 1;
            } else {
                spans.push(CellSpan {
                    text: character.to_string(),
                    columns: 1,
                    fg: cell.fg,
                    bg: cell.bg,
                    flags: cell.flags,
                    fg_rgb: cell.fg_rgb,
                    bg_rgb: cell.bg_rgb,
                    link_uri,
                    link_key,
                });
                open = Some(spans.len() - 1);
            }
            col += 1;
            continue;
        }
        open = None;

        let mut columns = 1u32;
        if cell.width >= 2 {
            // The lead owns every continuation column that follows it. A lead
            // in the last column of a truncated row has none and occupies one.
            while (col + columns as usize) < end && cells[col + columns as usize].width == 0 {
                columns += 1;
            }
        }
        let text = if cell.width == 0 {
            " ".to_owned()
        } else {
            scalar_text(cell)
        };
        spans.push(CellSpan {
            text,
            columns,
            fg: cell.fg,
            bg: cell.bg,
            flags: cell.flags,
            fg_rgb: cell.fg_rgb,
            bg_rgb: cell.bg_rgb,
            link_uri,
            link_key,
        });
        col += columns as usize;
    }
    spans
}

/// A cell's text, with its zero-width characters appended so a cluster stays
/// one span and is never sliced.
fn scalar_text(cell: &CellData) -> String {
    let mut text = String::new();
    if let Some(character) = char::from_u32(cell.character) {
        text.push(character);
    }
    if let Some(combining) = &cell.combining {
        for code_point in combining {
            if let Some(character) = char::from_u32(*code_point) {
                text.push(character);
            }
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::{row_to_spans, same_run};
    use crate::core::CellData;

    fn linked(text: char) -> CellData {
        CellData {
            character: text as u32,
            link_uri: Some("https://example.test/doc".to_owned()),
            link_key: Some("k-1".to_owned()),
            ..Default::default()
        }
    }

    #[test]
    fn two_cells_of_one_link_extend_the_same_run() {
        let cells = vec![linked('a'), linked('b')];
        let spans = row_to_spans(&cells, cells.len());
        assert_eq!(spans.len(), 1, "one link run is one span: {spans:?}");
        assert_eq!(spans[0].text, "ab");
        assert_eq!(spans[0].columns, 2);
    }

    #[test]
    fn the_comparison_itself_accepts_a_matching_neighbour() {
        let cells = vec![linked('a')];
        let spans = row_to_spans(&cells, cells.len());
        let run = spans.first().expect("one span");
        assert!(
            same_run(&linked('b'), run, Some("k-1")),
            "an identical linked cell must extend the run"
        );
    }
}
