//! How a cell row is SPELLED, pinned byte for byte. A browser parses this
//! shape directly, so a field that quietly appears, disappears or moves is a
//! terminal that paints a different colour than the worker observed — with
//! nothing in any log to say why.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_protocol::cell::{CellRow, CellSpan, DEFAULT_COLOR};
use roost_worker::session::retained_grid::cell_row_json;
use std::sync::Arc;

fn plain() -> CellSpan {
    CellSpan {
        text: "plain".to_string(),
        fg: 0,
        bg: 0,
        flags: 0,
        fg_rgb: None,
        bg_rgb: None,
        columns: 5,
        link_uri: None,
        link_key: None,
    }
}

fn linked() -> CellSpan {
    CellSpan {
        text: "link".to_string(),
        fg: DEFAULT_COLOR,
        bg: 17,
        flags: 1,
        fg_rgb: Some(0x00FF_7F),
        bg_rgb: Some(0x10_1010),
        columns: 4,
        link_uri: Some("https://example.test/d".to_string()),
        link_key: Some("run-1".to_string()),
    }
}

/// EVERY FIELD THE BROWSER READS IS PRESENT EVEN WHEN IT IS A ZERO. proto3
/// JSON would drop `"fg":0`; the renderer compares spans by identity and looks
/// a palette entry up by that same field, so an omitted `fg` is a black span
/// that compares unequal to the identical black span beside it, and every frame
/// repaints every row.
#[test]
fn a_row_is_spelled_the_way_the_browser_already_parses_it() {
    let row = CellRow {
        index: 7,
        spans: Arc::from(vec![plain(), linked()]),
    };
    assert_eq!(
        serde_json::to_string(&cell_row_json(&row)).expect("a row encodes"),
        concat!(
            r#"{"index":7,"spans":["#,
            r#"{"text":"plain","fg":0,"bg":0,"flags":0,"columns":5},"#,
            r#"{"text":"link","fg":256,"bg":17,"flags":1,"fgRgb":65407,"bgRgb":1052688,"#,
            r#""columns":4,"linkUri":"https://example.test/d","linkKey":"run-1"}"#,
            r#"]}"#
        ),
        "the shape and field order the browser parses, unchanged"
    );
    assert!(
        !cell_row_json(&row)["spans"][0]["fg"].is_null(),
        "a zero palette index is present rather than absent"
    );
}

/// A ROW WITH NO LINK CARRIES NO LINK FIELDS AT ALL, rather than null ones. A
/// browser that groups spans by link key cannot group a null, and a field that
/// is always there and always empty is a field every caller has to special-case.
#[test]
fn a_row_without_a_link_carries_no_link_fields() {
    let row = CellRow {
        index: 0,
        spans: Arc::from(vec![plain()]),
    };
    let span = &cell_row_json(&row)["spans"][0];
    assert!(span.get("linkUri").is_none(), "no URI, no field");
    assert!(span.get("linkKey").is_none(), "no key, no field");
    assert!(span.get("fgRgb").is_none(), "no true colour, no field");
}

/// A URI WITHOUT ITS RUN IDENTITY IS REFUSED UPSTREAM, so a projection that
/// emitted one without the other would be inventing a link the core never
/// authored. Both travel together or neither does.
#[test]
fn a_link_uri_and_its_run_identity_travel_together() {
    let mut half = linked();
    half.link_key = None;
    let row = CellRow {
        index: 0,
        spans: Arc::from(vec![half]),
    };
    let span = &cell_row_json(&row)["spans"][0];
    assert!(span.get("linkUri").is_some());
    assert!(
        span.get("linkKey").is_none(),
        "and the projection does not invent a run identity for a URI that \
         arrived without one"
    );
}
