//! Direct scrollback keeps its authorization predicate live across the
//! authoritative reader: a grant reduction after the read was admitted returns
//! no row payload even when the reader completed a valid page. Ports
//! `apps/worker/tests/local-door/local-terminal-scrollback.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod terminal_stream_support;

use std::sync::atomic::{AtomicUsize, Ordering};

use roost_proto::LocalScrollbackRequest;
use roost_term::RioCore;
use roost_worker::local_terminal::read_local_scrollback;
use terminal_stream_support::{COLS, Harness, ROWS, SESSION};

#[tokio::test]
async fn post_read_authority_loss_suppresses_the_direct_scrollback_page() {
    let harness = Harness::new(RioCore::new(COLS, ROWS));
    let checks = AtomicUsize::new(0);
    let request = LocalScrollbackRequest {
        request_id: "history-authority-loss".to_owned(),
        session_id: SESSION.to_owned(),
        grid_epoch: String::new(),
        end_row: 0,
        max_rows: 1,
        ..Default::default()
    };

    let allows = |_: &str| checks.fetch_add(1, Ordering::SeqCst) == 0;
    let response = read_local_scrollback(&harness.manager, &harness.table, &request, &allows).await;

    assert_eq!(
        checks.load(Ordering::SeqCst),
        2,
        "admitted once, re-checked once after the read"
    );
    assert_eq!(response.request_id, "history-authority-loss");
    assert_eq!(response.error, "terminal session is unavailable");
    assert!(response.rows.is_empty());
}

#[tokio::test]
async fn a_page_names_its_rows_and_the_grid_it_was_read_from() {
    let harness = Harness::new(RioCore::new(COLS, ROWS));
    for line in 0..20 {
        harness.deliver(format!("line-{line}\r\n").as_bytes());
    }
    let request = LocalScrollbackRequest {
        request_id: "page".to_owned(),
        session_id: SESSION.to_owned(),
        end_row: 10,
        max_rows: 4,
        ..Default::default()
    };

    let response =
        read_local_scrollback(&harness.manager, &harness.table, &request, &|_: &str| true).await;

    assert_eq!(response.error, "");
    assert_eq!((response.start_row, response.end_row), (6, 10));
    assert_eq!(response.rows.len(), 4);
    assert!(!response.grid_epoch.is_empty());
    assert!(response.scrollback_total >= 10);
}
