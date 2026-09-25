//! The run-length span encoder on its own: how a row of cells folds into
//! spans, and the two rules that keep a row column-aligned.

use std::sync::Arc;

use roost_protocol::cell::CellSpan;
use roost_term::{AlacrittyCore, TerminalCore, grid_to_cell_frame, row_to_spans};

/// The terminal columns a row's spans occupy.
fn column_total(spans: &[CellSpan]) -> u32 {
    spans.iter().map(|span| span.columns).sum()
}

#[test]
fn the_span_encoder_never_loses_a_column_to_a_wide_glyphs_tail() {
    // A lead whose continuation is trailing is the second column of that
    // glyph, not padding, so trimming it would shrink the lead.
    let cells = vec![
        roost_term::CellData {
            character: 'x' as u32,
            ..Default::default()
        },
        roost_term::CellData {
            character: '中' as u32,
            width: 2,
            ..Default::default()
        },
        roost_term::CellData {
            character: 0,
            width: 0,
            ..Default::default()
        },
    ];
    let spans: Vec<CellSpan> = row_to_spans(&cells, cells.len());
    let total: u32 = spans.iter().map(|span| span.columns).sum();
    assert_eq!(total, 3, "every column is accounted for");
    assert_eq!(spans[1].columns, 2, "the lead keeps both of its columns");
}

#[test]
fn an_orphan_continuation_still_occupies_its_column() {
    // A continuation with no lead before it, followed by text so it is not at
    // the trailing edge: it must paint as one blank column rather than vanish,
    // or the row would be one column short and everything after it would move.
    let cells = vec![
        roost_term::CellData {
            character: 0,
            width: 0,
            ..Default::default()
        },
        roost_term::CellData {
            character: 'a' as u32,
            ..Default::default()
        },
    ];
    let spans: Vec<CellSpan> = row_to_spans(&cells, cells.len());
    assert_eq!(spans.len(), 2, "the orphan and the text after it");
    assert_eq!(spans[0].columns, 1, "an orphan occupies one column");
    assert_eq!(spans[0].text, " ");
    assert_eq!(spans[1].text, "a");
    assert_eq!(column_total(&spans), 2, "the row spans exactly its cells");
}

#[test]
fn span_arrays_are_shared_not_copied_on_a_clone() {
    // The clone of a frame must keep sharing its cells, or a 10k-row frame
    // costs a full deep copy every time a replica reconciles it.
    let mut core = AlacrittyCore::new(20, 5);
    core.write(b"hello");
    let frame = grid_to_cell_frame(&core, 1, "g:0", "s", Some(0), 0);
    let cloned = frame.viewport_rows[0].spans.clone();
    assert!(
        Arc::ptr_eq(&frame.viewport_rows[0].spans, &cloned),
        "a cloned frame must share its cells, not deep-copy them"
    );
}
