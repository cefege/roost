//! Painting one `CellRow` into one element: its spans, its find-match
//! sub-spans, and one anchor per maximal run of spans sharing a link run key.
//!
//! `cell_row.rs` decides the CSS and the slicing; this module turns that into
//! nodes. Every piece of a link run — including the sub-spans a find match
//! splits a span into — is appended INSIDE that run's anchor, so a highlighted
//! link stays one clickable element and both halves of a split span keep their
//! link.

use crate::cell_renderer_dom::DomResult;
use crate::cell_row::{
    FindHit, LINK_KEY_ATTR, ROW_COLUMNS_ATTR, ROW_HAS_LINKS_ATTR, SpanSlice, TERMINAL_LINK_CLASS,
    TERMINAL_LINK_TARGET_ATTR, row_column_count, slice_text, span_decoration_style, span_slices,
    span_style,
};
use crate::link_target::{TerminalLinkTarget, classify_terminal_link_target};
use crate::render_element::RenderElement;
use roost_protocol::cell::{CellRow, CellSpan};

/// Paint one row into a fresh element whose class is exactly `cell-row`,
/// created in `factory`'s document.
///
/// A row with no spans still gets one text node, because a blank row with no
/// content has no height and the grid's rows would stop being a regular pitch.
pub fn render_row<E: RenderElement>(
    row: &CellRow,
    factory: &E,
    hits: Option<&[FindHit]>,
    active_col: Option<u32>,
) -> DomResult<E> {
    let element = factory.create_element("div")?;
    element.set_class_name("cell-row");
    element.set_attribute(ROW_COLUMNS_ATTR, &row_column_count(row).to_string());
    if row.spans.is_empty() {
        element.set_text(" ");
        return Ok(element);
    }
    // A row carries no highlight class at all until a match actually lands in
    // it, so an unhighlighted row is byte-identical to one painted before any
    // find was typed.
    let marked = hits.is_some_and(|hits| !hits.is_empty());
    let mut column = 0u32;
    let mut anchor: Option<E> = None;
    let mut anchor_key = String::new();
    for span in row.spans.iter() {
        let host = match span.link_uri.as_deref() {
            None => {
                anchor = None;
                anchor_key.clear();
                element.clone()
            }
            Some(uri) => {
                // The run key is present exactly when the URI is; the URI is a
                // total fallback so a malformed frame still has stable runs.
                let key = span.link_key.as_deref().unwrap_or(uri);
                if anchor.is_none() || key != anchor_key {
                    let opened = build_link_anchor(factory, uri, key)?;
                    if let Some(opened) = opened.as_ref() {
                        element.set_attribute(ROW_HAS_LINKS_ATTR, "1");
                        element.append_child(opened);
                    }
                    anchor = opened;
                    anchor_key = if anchor.is_some() {
                        key.to_string()
                    } else {
                        String::new()
                    };
                }
                anchor.clone().unwrap_or_else(|| element.clone())
            }
        };
        column = paint_span(
            span,
            &host,
            if marked { hits } else { None },
            active_col,
            column,
        )?;
    }
    Ok(element)
}

/// The class a find-match piece carries. The active match gets a second class
/// so a stylesheet can move the current match without re-deriving it from the
/// hit list.
pub fn find_hit_class(active: bool) -> &'static str {
    if active {
        "cell-find-hit cell-find-hit-active"
    } else {
        "cell-find-hit"
    }
}

/// Paint one span into `host`, returning the grid column after it.
fn paint_span<E: RenderElement>(
    span: &CellSpan,
    host: &E,
    hits: Option<&[FindHit]>,
    active_col: Option<u32>,
    column: u32,
) -> DomResult<u32> {
    let run_style = span_style(span);
    let decoration = span_decoration_style(span);
    let Some(hits) = hits else {
        let text = slice_text(span, 0, span.columns);
        append_slice(host, &run_style, None, &text)?;
        return Ok(column + span.columns);
    };
    for slice in span_slices(span, hits, active_col) {
        // A highlighted piece hands colour to the `.cell-find-hit` class and
        // keeps only the run's DECORATION, because an inline colour would beat
        // the class rule and leave matches on styled output un-highlighted.
        let style = if slice.highlighted {
            &decoration
        } else {
            &run_style
        };
        let text = slice_text(span, slice.start, slice.columns);
        append_slice(host, style, Some(slice), &text)?;
    }
    Ok(column + span.columns)
}

fn append_slice<E: RenderElement>(
    host: &E,
    style: &str,
    slice: Option<SpanSlice>,
    text: &str,
) -> DomResult<()> {
    let element = host.create_element("span")?;
    if let Some(slice) = slice.filter(|slice| slice.highlighted) {
        element.set_class_name(find_hit_class(slice.active));
    }
    if !style.is_empty() {
        element.set_attribute("style", style);
    }
    element.set_text(text);
    host.append_child(&element);
    Ok(())
}

/// Build one anchor for a link run, or `None` when the target is not one this
/// build will ever open.
///
/// Terminal output is untrusted: an external anchor exists solely for an
/// absolute HTTP(S) target, and a file target carries no browser-openable href
/// until the worker-aware link attachment resolves it against the current
/// worker and cwd.
fn build_link_anchor<E: RenderElement>(
    factory: &E,
    raw_target: &str,
    key: &str,
) -> DomResult<Option<E>> {
    let Some(target) = classify_terminal_link_target(raw_target, None) else {
        return Ok(None);
    };
    let anchor = factory.create_element("a")?;
    anchor.set_class_name(TERMINAL_LINK_CLASS);
    anchor.set_attribute(LINK_KEY_ATTR, key);
    anchor.set_attribute(TERMINAL_LINK_TARGET_ATTR, raw_target);
    anchor.set_attribute("tabindex", "-1");
    anchor.set_attribute("draggable", "false");
    match target {
        TerminalLinkTarget::External { href, display } => {
            anchor.set_attribute("href", &href);
            anchor.set_attribute("target", "_blank");
            anchor.set_attribute("rel", "noopener noreferrer");
            anchor.set_attribute("data-hint", &display);
        }
        TerminalLinkTarget::WorkerFile { display, .. } => {
            anchor.set_attribute("data-kind", "file");
            anchor.set_attribute("data-hint", &format!("Open {display}"));
        }
    }
    Ok(Some(anchor))
}
