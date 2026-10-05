//! The inline-style strings a painted span carries, memoised by the span fields
//! that decide them, so a frame repainting runs of one style allocates each
//! style string once. Owned by `CellGridRenderer`, passed to `cell_row::dom`'s
//! `render_row`; the strings themselves are `cell_row`'s `span_style` and
//! `span_decoration_style`.

use std::collections::HashMap;
use std::rc::Rc;

use roost_protocol::cell::{CellSpan, span_is_atomic};

use crate::cell_row::{span_decoration_style, span_style};

/// Entries per map before it is dropped wholesale. A terminal's live palette is
/// a few dozen styles; a truecolor gradient is the case that would grow forever.
pub const STYLE_CACHE_CAP: usize = 2_048;

/// The memoised run and decoration styles.
#[derive(Debug, Default)]
pub struct StyleCache {
    runs: HashMap<SpanStyleKey, Rc<str>>,
    decorations: HashMap<SpanStyleKey, Rc<str>>,
}

/// Every span field the two style functions read. Text matters only through
/// whether the span is an atomic box, and the column count only then.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SpanStyleKey {
    fg: u16,
    fg_rgb: Option<u32>,
    bg: u16,
    bg_rgb: Option<u32>,
    flags: u16,
    atomic_columns: Option<u32>,
}

impl SpanStyleKey {
    fn of(span: &CellSpan) -> Self {
        Self {
            fg: span.fg,
            fg_rgb: span.fg_rgb,
            bg: span.bg,
            bg_rgb: span.bg_rgb,
            flags: span.flags,
            atomic_columns: span_is_atomic(span).then_some(span.columns),
        }
    }
}

impl StyleCache {
    /// `span_style(span)`, built at most once per distinct style.
    pub fn run_style(&mut self, span: &CellSpan) -> Rc<str> {
        cached(&mut self.runs, span, span_style)
    }

    /// `span_decoration_style(span)`, built at most once per distinct style.
    pub fn decoration_style(&mut self, span: &CellSpan) -> Rc<str> {
        cached(&mut self.decorations, span, span_decoration_style)
    }
}

fn cached(
    map: &mut HashMap<SpanStyleKey, Rc<str>>,
    span: &CellSpan,
    build: fn(&CellSpan) -> String,
) -> Rc<str> {
    let key = SpanStyleKey::of(span);
    if let Some(style) = map.get(&key) {
        return Rc::clone(style);
    }
    if map.len() >= STYLE_CACHE_CAP {
        map.clear();
    }
    let style: Rc<str> = Rc::from(build(span));
    map.insert(key, Rc::clone(&style));
    style
}
