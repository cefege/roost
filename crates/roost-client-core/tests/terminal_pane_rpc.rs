//! A terminal pane's two direct calls encode the fields v2's call sites send,
//! and a history page decodes to exactly the rows and floor the coordinator
//! served — a page with an unreadable row is an error, never a shorter page.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::client::rpc::UnaryMethod;
use roost_client_core::client::rpc::calls::terminal_pane::{CursorPos, ScrollbackCells};
use roost_proto::buffa::Message;
use roost_proto::{
    PbCellRow, PbCellSpan, ScrollbackHistoryFloor as PbFloor, SessionsCursorPosRequest,
    SessionsGetScrollbackCellsRequest, SessionsGetScrollbackCellsResponse,
};
use roost_protocol::terminal_search::ScrollbackHistoryFloor;

fn row(index: u32, text: &str, fg: u32) -> PbCellRow {
    PbCellRow {
        index,
        spans: vec![PbCellSpan {
            text: text.to_owned(),
            fg,
            columns: u32::try_from(text.len()).unwrap(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

#[test]
fn a_history_request_names_its_newest_row_and_the_painted_epoch() {
    let call = ScrollbackCells {
        session_id: "s-1".to_owned(),
        end_row: 400,
        max_rows: 250,
        grid_epoch: "epoch-7".to_owned(),
    };
    let sent =
        SessionsGetScrollbackCellsRequest::decode_from_slice(&call.encode_request().unwrap())
            .unwrap();
    assert_eq!(
        (sent.session_id.as_str(), sent.end_row, sent.max_rows, sent.grid_epoch.as_str()),
        ("s-1", 400, 250, "epoch-7")
    );
}

#[test]
fn a_history_page_decodes_its_rows_and_its_floor() {
    let body = SessionsGetScrollbackCellsResponse {
        rows: vec![row(150, "old", 1), row(151, "new", 2)],
        cols: 80,
        scrollback_total: 900,
        start_row: 150,
        end_row: 152,
        grid_epoch: "epoch-7".to_owned(),
        history_floor: PbFloor::Evicted.into(),
        ..Default::default()
    }
    .encode_to_vec();
    let page = ScrollbackCells::decode_response(&body).unwrap();
    assert_eq!(page.rows.iter().map(|row| row.index).collect::<Vec<_>>(), [150, 151]);
    assert_eq!((page.start_row, page.end_row, page.cols), (150, 152, 80));
    assert_eq!(page.scrollback_total, 900);
    assert_eq!(page.grid_epoch, "epoch-7");
    assert_eq!(page.history_floor, ScrollbackHistoryFloor::Evicted);

    let unclaimed = SessionsGetScrollbackCellsResponse::default().encode_to_vec();
    assert_eq!(
        ScrollbackCells::decode_response(&unclaimed).unwrap().history_floor,
        ScrollbackHistoryFloor::None
    );
}

#[test]
fn a_page_with_an_unreadable_row_is_refused_whole() {
    let body = SessionsGetScrollbackCellsResponse {
        rows: vec![row(10, "fine", 1), row(11, "bad", 9_999)],
        start_row: 10,
        end_row: 12,
        ..Default::default()
    }
    .encode_to_vec();
    assert!(ScrollbackCells::decode_response(&body).is_err());
}

#[test]
fn a_cursor_report_carries_the_column_and_row() {
    let call = CursorPos {
        session_id: "s-2".to_owned(),
        col: 12,
        row: 3,
    };
    let sent = SessionsCursorPosRequest::decode_from_slice(&call.encode_request().unwrap()).unwrap();
    assert_eq!((sent.session_id.as_str(), sent.col, sent.row), ("s-2", 12, 3));
}
