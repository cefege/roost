//! The window a scrollback page names, and the order its rows must arrive in.
//!
//! The window arithmetic is the coordinator's own; the worker's read of the
//! same range lives in `roost-worker` and neither crate imports the other. The
//! two are checked against each other only through the wire, which is why the
//! row-order and contiguity assertions here are the contract rather than an
//! implementation detail. What a search page may say is
//! `terminal_screen_search_result.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use connectrpc::ErrorCode;
use roost_coord::terminal_screen::rpc_relay::decode_cells_page;
use roost_coord::terminal_screen::scrollback_window::{
    ScrollbackRowOrder, ScrollbackWindow, check_row_order, require_json_safe_row,
};

#[test]
fn a_page_window_is_the_max_rows_immediately_before_the_exclusive_end() {
    let window = ScrollbackWindow::for_request(500, 100, "scrollback cells end_row").unwrap();
    assert_eq!(
        (window.start_row, window.end_row, window.row_count()),
        (400, 500, 100)
    );
    assert!(window.is_served_by(400, 500), "the exact window is served");
    assert!(
        window.is_served_by(450, 500),
        "a page SHORT of the request is legal: the worker clamped at its retained floor, \
         and `history_floor` is how it says so"
    );
    assert!(
        !window.is_served_by(300, 501),
        "a page that runs past the exclusive end the caller named is refused: those rows \
         were never asked for"
    );
    assert!(
        window.is_served_by(400, 499),
        "a page that ends early is short, not wrong"
    );
}

#[test]
fn a_page_window_below_the_first_row_is_clamped_rather_than_going_negative() {
    let window = ScrollbackWindow::for_request(30, 100, "scrollback cells end_row").unwrap();
    assert_eq!(
        (window.start_row, window.end_row, window.row_count()),
        (0, 30, 30),
        "a session that has never scrolled has no row below zero, and a negative start would \
         name a row no core can serve"
    );
}

#[test]
fn a_row_index_a_browser_cannot_address_is_refused_rather_than_rounded() {
    assert!(require_json_safe_row(9_007_199_254_740_991, "end_row").is_ok());
    let error = require_json_safe_row(9_007_199_254_740_992, "end_row").unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert!(error.message.unwrap().contains("end_row"));
}

#[test]
fn served_rows_must_be_one_ascending_contiguous_run_from_the_served_start() {
    assert_eq!(
        check_row_order(10, &[10, 11, 12]),
        ScrollbackRowOrder::Ordered
    );
    assert_eq!(
        check_row_order(10, &[]),
        ScrollbackRowOrder::Ordered,
        "an empty page is still in order"
    );
    assert!(matches!(
        check_row_order(10, &[12, 11, 10]),
        ScrollbackRowOrder::Refused(_)
    ));
    assert!(matches!(
        check_row_order(10, &[10, 12]),
        ScrollbackRowOrder::Refused(_)
    ));
    assert!(matches!(
        check_row_order(10, &[9]),
        ScrollbackRowOrder::Refused(_)
    ));
}

// The browser pages history 250 rows at a time (smoke "streaming sequence
// repair leaves an off-bottom reader fixed"); v2 relays the worker's page whole
// (`handlers-sessions-scrollback.ts:216-226`), so a full page is served whole.
#[test]
fn a_full_browser_page_of_history_is_served_whole() {
    let window = ScrollbackWindow::for_request(1_397, 250, "scrollback cells end_row").unwrap();
    let rows: Vec<serde_json::Value> = (1_147..1_397)
        .map(|index| serde_json::json!({ "index": index, "spans": [] }))
        .collect();
    let payload = serde_json::json!({
        "rows": rows,
        "start_row": 1_147,
        "end_row": 1_397,
        "cols": 110,
        "total": 1_500,
        "grid_epoch": "epoch-1",
    });
    let page = decode_cells_page(&payload, &window).unwrap();
    assert_eq!(page.rows.len(), 250);
    assert_eq!((page.start_row, page.end_row), (1_147, 1_397));
    assert_eq!(page.rows.last().map(|row| row.index), Some(1_396));
}
