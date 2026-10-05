//! The style memo: equal style fields share one string, any differing field
//! does not, and an overfull map clears itself and keeps answering.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::rc::Rc;

use roost_protocol::cell::{CELL_BOLD, CellSpan, DEFAULT_COLOR};
use roost_web_terminal::cell_row::style_cache::{STYLE_CACHE_CAP, StyleCache};
use roost_web_terminal::cell_row::{span_decoration_style, span_style};

fn span(text: &str, fg: u16, flags: u16) -> CellSpan {
    CellSpan {
        text: text.to_owned(),
        fg,
        bg: DEFAULT_COLOR,
        flags,
        fg_rgb: None,
        bg_rgb: None,
        columns: text.chars().count() as u32,
        link_uri: None,
        link_key: None,
    }
}

#[test]
fn equal_style_fields_share_one_string_whatever_the_text() {
    let mut cache = StyleCache::default();
    let first = cache.run_style(&span("hello", 2, CELL_BOLD));
    let second = cache.run_style(&span("other words", 2, CELL_BOLD));
    assert!(Rc::ptr_eq(&first, &second));
    assert_eq!(&*first, span_style(&span("x", 2, CELL_BOLD)));
    let decoration = cache.decoration_style(&span("hello", 2, CELL_BOLD));
    assert_eq!(
        &*decoration,
        span_decoration_style(&span("x", 2, CELL_BOLD))
    );
}

#[test]
fn one_differing_field_is_a_different_entry() {
    let mut cache = StyleCache::default();
    let bold = cache.run_style(&span("a", 2, CELL_BOLD));
    let plain = cache.run_style(&span("a", 2, 0));
    let recoloured = cache.run_style(&span("a", 3, CELL_BOLD));
    assert!(!Rc::ptr_eq(&bold, &plain));
    assert!(!Rc::ptr_eq(&bold, &recoloured));
}

#[test]
fn past_the_cap_the_cache_clears_and_keeps_answering() {
    let mut cache = StyleCache::default();
    let truecolor = |rgb: u32| CellSpan {
        fg_rgb: Some(rgb),
        ..span("a", 2, 0)
    };
    let first = cache.run_style(&truecolor(0));
    for rgb in 1..=STYLE_CACHE_CAP as u32 {
        cache.run_style(&truecolor(rgb));
    }
    let again = cache.run_style(&truecolor(0));
    assert!(!Rc::ptr_eq(&first, &again), "the overfull map was dropped");
    assert_eq!(first, again);
    assert_eq!(&*again, span_style(&truecolor(0)));
}
