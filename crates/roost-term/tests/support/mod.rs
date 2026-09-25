//! Helpers shared by the terminal core's test files.
//!
//! A row's text and a row's column count are different things: a wide glyph is
//! one character of text occupying two terminal columns, and a helper that
//! conflated them would hide exactly the property these tests exist to check.

use roost_protocol::cell::CellSpan;

/// The terminal columns a row's spans occupy.
pub fn row_columns(spans: &[CellSpan]) -> u32 {
    spans.iter().map(|span| span.columns).sum()
}
