//! The smoke backdoor's scans and geometry: painted-marker accounting, the
//! retained-scrollback pager, the render probe's row reading, and the paint
//! proof's rectangle/range/style rules. Pins `smoke::{marker_scan,
//! retained_scan, probes, paint_proof}` (v2 `smokeTerminalRenderProbes.ts`,
//! `smokeRetainedMarkerScan.ts`, `smokeHarness.ts`).
#![cfg(feature = "smoke")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use roost_client_core::client::rpc::calls::terminal_pane::ScrollbackCellsPage;
use roost_protocol::cell::{CellRow, CellSpan};
use roost_protocol::terminal_search::ScrollbackHistoryFloor;
use roost_web::smoke::marker_scan::{prefixed_markers, scan_painted_rows};
use roost_web::smoke::paint_proof::{
    RectSnapshot, background_is_transparent, cursor_aligned, dataset_coordinate, marker_text_range,
    style_hides,
};
use roost_web::smoke::probes::{GridScrollBox, SmokeRenderProbe, parse_cell_cols};
use roost_web::smoke::retained_scan::{RetainedScanPager, retained_page_rows};

#[test]
fn a_marker_scan_reports_depth_duplication_loss_and_the_first_inversion() {
    let scan = scan_painted_rows(["M1 M2", "M4", "M2", "M3"], "M");
    assert_eq!((scan.total, scan.unique, scan.min, scan.max), (5, 4, 1, 4));
    assert_eq!(scan.duplicated, vec![2]);
    assert_eq!(scan.missing, 0);
    assert_eq!((scan.out_of_order, scan.first_inversion), (1, 2));

    let gapped = scan_painted_rows(["M1", "M3", "M6"], "M");
    assert_eq!(gapped.missing, 3);
    let empty = scan_painted_rows(["no markers"], "M");
    assert_eq!((empty.total, empty.min, empty.max, empty.first_inversion), (0, 0, 0, -1));
}

#[test]
fn the_prefix_is_literal_and_needs_digits_after_it() {
    assert_eq!(prefixed_markers("a.b1 axb2 a.b3", "a.b"), vec![1, 3]);
    assert_eq!(prefixed_markers("MM1 M M22", "M"), vec![1, 22]);
    assert_eq!(prefixed_markers("é-M7", "M"), vec![7]);
}

#[test]
fn the_marker_scan_answers_the_oracles_field_names() {
    let scan = serde_json::to_value(scan_painted_rows(["M2", "M1"], "M")).unwrap();
    assert_eq!(scan["outOfOrder"], 1);
    assert_eq!(scan["firstInversion"], 1);
    assert_eq!(scan["duplicated"], serde_json::json!([]));
}

fn span(text: &str) -> CellSpan {
    CellSpan {
        text: text.to_owned(),
        fg: 256,
        bg: 256,
        flags: 0,
        fg_rgb: None,
        bg_rgb: None,
        columns: u32::try_from(text.len()).unwrap(),
        link_uri: None,
        link_key: None,
    }
}

fn page(start: u64, texts: &[&str], total: u64, floor: ScrollbackHistoryFloor) -> ScrollbackCellsPage {
    let rows = texts
        .iter()
        .enumerate()
        .map(|(offset, text)| CellRow {
            index: u32::try_from(start).unwrap() + u32::try_from(offset).unwrap(),
            spans: Arc::from(vec![span(text)]),
        })
        .collect();
    ScrollbackCellsPage {
        rows,
        cols: 80,
        scrollback_total: total,
        start_row: start,
        end_row: start + texts.len() as u64,
        grid_epoch: "g-1".to_owned(),
        history_floor: floor,
    }
}

#[test]
fn the_retained_scan_pages_newest_first_and_reassembles_history_in_order() {
    let mut pager = RetainedScanPager::new("s1", "g-1", 2).unwrap();
    assert_eq!(pager.next_request().unwrap().end_row, 9_007_199_254_740_991);
    assert!(!pager.accept(&page(3, &["R4", "R5"], 5, ScrollbackHistoryFloor::None)).unwrap());
    let second = pager.next_request().unwrap();
    assert_eq!((second.end_row, second.max_rows, second.grid_epoch.as_str()), (3, 2, "g-1"));
    assert!(!pager.accept(&page(1, &["R2", "R3"], 5, ScrollbackHistoryFloor::None)).unwrap());
    assert!(pager.accept(&page(0, &["R1"], 5, ScrollbackHistoryFloor::Evicted)).unwrap());
    let scan = pager.finish("R");
    assert_eq!(scan.row_indices, vec![0, 1, 2, 3, 4]);
    assert_eq!(scan.marker_ids, vec![1, 2, 3, 4, 5]);
    assert_eq!((scan.pages, scan.row_gap_count, scan.marker_missing), (3, 0, 0));
    assert_eq!((scan.retained_floor, scan.retained_cap), (0, 5));
    assert_eq!(scan.retained_floor_reason, "evicted");
}

#[test]
fn a_short_page_sets_the_floor_and_its_reason() {
    let mut pager = RetainedScanPager::new("s1", "g-1", 4).unwrap();
    assert!(pager.accept(&page(6, &[], 10, ScrollbackHistoryFloor::ResizeReplay)).unwrap());
    let scan = pager.finish("R");
    assert_eq!((scan.retained_floor, scan.retained_cap), (6, 4));
    assert_eq!(scan.retained_floor_reason, "resize_replay");
    assert!(scan.row_indices.is_empty());
}

#[test]
fn a_moving_or_malformed_snapshot_fails_the_scan_instead_of_passing() {
    let mut moved = RetainedScanPager::new("s1", "g-1", 2).unwrap();
    moved.accept(&page(3, &["R4", "R5"], 5, ScrollbackHistoryFloor::None)).unwrap();
    let error = moved.accept(&page(1, &["R2", "R3"], 6, ScrollbackHistoryFloor::None)).unwrap_err();
    assert!(error.contains("scrollback changed"), "{error}");

    let mut foreign = RetainedScanPager::new("s1", "g-1", 2).unwrap();
    let mut other_epoch = page(3, &["R4"], 5, ScrollbackHistoryFloor::None);
    other_epoch.grid_epoch = "g-2".to_owned();
    assert!(foreign.accept(&other_epoch).unwrap_err().contains("invalid retained marker page"));

    let mut holed = RetainedScanPager::new("s1", "g-1", 2).unwrap();
    let mut skipped = page(3, &["R4", "R5"], 5, ScrollbackHistoryFloor::None);
    skipped.rows[1].index = 7;
    assert!(holed.accept(&skipped).unwrap_err().contains("non-contiguous retained page for s1 at 4"));

    let mut stuck = RetainedScanPager::new("s1", "g-1", 2).unwrap();
    stuck.accept(&page(3, &["R4", "R5"], 9, ScrollbackHistoryFloor::None)).unwrap();
    assert!(stuck.accept(&page(3, &["R4", "R5"], 9, ScrollbackHistoryFloor::None)).unwrap_err().contains("no progress"));
}

#[test]
fn the_retained_scan_refuses_a_bad_page_size_and_a_missing_epoch_and_bounds_its_pages() {
    assert_eq!(retained_page_rows(None), Ok(512));
    for bad in [0.0, 4097.0, 1.5] {
        assert!(retained_page_rows(Some(bad)).is_err(), "{bad}");
    }
    assert!(RetainedScanPager::new("s1", "", 2).unwrap_err().contains("no cell grid epoch for s1"));

    let mut endless = RetainedScanPager::new("s1", "g-1", 1).unwrap();
    for start in (1..=128_u64).rev() {
        endless.next_request().unwrap();
        endless.accept(&page(start + 1000, &["x"], 2000, ScrollbackHistoryFloor::None)).unwrap();
    }
    assert!(endless.next_request().unwrap_err().contains("exceeded 128 pages"));
}

#[test]
fn the_render_probe_reads_painted_rows_the_way_the_oracle_compares_them() {
    let rows = vec!["\u{a0}\u{a0}".to_owned(), "$ ls\u{a0}  ".to_owned(), "out".to_owned(), "   ".to_owned()];
    let scroll = GridScrollBox { scroll_top: 100.4, scroll_height: 500.0, client_height: 400.0 };
    let probe = SmokeRenderProbe::of_grid(scroll, &rows);
    assert_eq!((probe.row_count, probe.non_empty_rows), (4, 2));
    assert_eq!((probe.first_line.as_str(), probe.last_line.as_str()), ("$ ls", "out"));
    assert_eq!((probe.scroll_top, probe.from_bottom), (100, 0));
    assert!(probe.at_bottom);
    let parked = SmokeRenderProbe::of_grid(GridScrollBox { scroll_top: 0.0, ..scroll }, &rows);
    assert!(!parked.at_bottom);
    assert_eq!(serde_json::to_value(parked).unwrap()["mode"], "cell");
    assert_eq!(serde_json::to_value(SmokeRenderProbe::absent()).unwrap()["found"], false);
}

#[test]
fn cell_columns_read_like_parse_int() {
    assert_eq!(parse_cell_cols("80"), 80);
    assert_eq!(parse_cell_cols(" 132px"), 132);
    assert_eq!((parse_cell_cols(""), parse_cell_cols("wide")), (0, 0));
}

#[test]
fn rectangles_intersect_only_with_area_and_clip_to_their_overlap() {
    let terminal = RectSnapshot::from_origin(0.0, 0.0, 100.0, 50.0);
    let marker = RectSnapshot::from_origin(90.0, 40.0, 20.0, 20.0);
    assert!(marker.intersects(&terminal));
    assert!(!RectSnapshot::from_origin(10.0, 10.0, 0.0, 5.0).intersects(&terminal));
    assert!(!RectSnapshot::from_origin(100.0, 0.0, 5.0, 5.0).intersects(&terminal));
    assert_eq!(marker.clipped_to(&terminal), Some(RectSnapshot::from_origin(90.0, 40.0, 10.0, 10.0)));
    assert!(RectSnapshot::from_origin(200.0, 0.0, 5.0, 5.0).clipped_to(&terminal).is_none());
    assert!(marker.stable_with(&RectSnapshot::from_origin(90.7, 40.0, 20.0, 20.0)));
    assert!(!marker.stable_with(&RectSnapshot::from_origin(90.8, 40.0, 20.0, 20.0)));
}

#[test]
fn a_marker_range_spans_text_nodes_in_utf16_offsets() {
    let nodes = vec!["😀 RO".to_owned(), "OST_1".to_owned(), " tail".to_owned()];
    let range = marker_text_range(&nodes, "ROOST_1").unwrap();
    assert_eq!((range.start_node, range.start_offset), (0, 3));
    assert_eq!((range.end_node, range.end_offset), (1, 5));
    assert!(marker_text_range(&nodes, "ROOST_2").is_none());
    assert!(marker_text_range(&nodes, "").is_none());
}

#[test]
fn visibility_and_cursor_paint_follow_computed_style() {
    assert!(style_hides("none", "visible", "1", "visible", true));
    assert!(style_hides("block", "collapse", "1", "visible", true));
    assert!(style_hides("block", "visible", "0", "visible", true));
    assert!(!style_hides("block", "visible", "0", "visible", false));
    assert!(!style_hides("block", "visible", "0.5", "auto", true));
    for clear in ["transparent", "rgba(0, 0, 0, 0)", "rgb(0 0 0 / 0)", "rgba(1,2,3,0.00)"] {
        assert!(background_is_transparent(clear), "{clear}");
    }
    for painted in ["rgb(255, 255, 255)", "rgba(10, 20, 30, 0.5)", "rgb(0 0 0 / 0.4)"] {
        assert!(!background_is_transparent(painted), "{painted}");
    }
}

#[test]
fn a_cursor_must_sit_on_its_row_and_column_and_carry_a_numeric_position() {
    let row = RectSnapshot::from_origin(10.0, 100.0, 800.0, 20.0);
    assert!(cursor_aligned(&RectSnapshot::from_origin(10.0 + 4.0 * 9.0, 100.0, 9.0, 20.0), &row, 4));
    assert!(!cursor_aligned(&RectSnapshot::from_origin(10.0 + 5.0 * 9.0, 100.0, 9.0, 20.0), &row, 4));
    assert!(!cursor_aligned(&RectSnapshot::from_origin(10.0, 104.0, 9.0, 20.0), &row, 0));
    assert_eq!(dataset_coordinate(None), None);
    assert_eq!(dataset_coordinate(Some("")), Some(0));
    assert_eq!(dataset_coordinate(Some(" 3 ")), Some(3));
    assert_eq!((dataset_coordinate(Some("-1")), dataset_coordinate(Some("1.5"))), (None, None));
}
