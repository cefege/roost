// The fleet clipboard history: what a copy stores, what it refuses, and what
// the caps and the retention window take away. Every device reads this list, so
// a duplicate, a row past the cap or a row past seven days is something every
// operator's sheet shows.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;
mod workspaces_support;

use connectrpc::ErrorCode;
use roost_coord::clipboard::{
    CLIPBOARD_HISTORY_LIMIT, CLIPBOARD_RETENTION_MS, CLIPBOARD_TEXT_MAX_BYTES, capture_osc52,
    handle_clipboard_add, handle_clipboard_clear, handle_clipboard_delete, handle_clipboard_list,
    prune_clipboard_history,
};

use workspaces_support::{SESSION_A, WORKER_FP, WorkspacesFixture, device_caller};

async fn list(fixture: &WorkspacesFixture) -> Vec<roost_proto::ClipboardEntry> {
    handle_clipboard_list(
        &fixture.core,
        &device_caller(),
        roost_proto::ClipboardListRequest::default(),
    )
    .await
    .expect("the list")
    .body
    .entries
}

async fn add(fixture: &WorkspacesFixture, text: &str) -> Result<(), ErrorCode> {
    handle_clipboard_add(
        &fixture.core,
        &device_caller(),
        roost_proto::ClipboardAddRequest {
            text: text.to_owned(),
            session_id: SESSION_A.to_owned(),
            source_kind: "selection".to_owned(),
            ..Default::default()
        },
    )
    .await
    .map(|_| ())
    .map_err(|error| error.code)
}

#[tokio::test]
async fn a_copy_is_listed_with_its_source_and_a_repeat_is_not_a_second_row() {
    let fixture = WorkspacesFixture::new("clipboard-add").await;
    fixture.enroll_session(SESSION_A, "/srv/one").await;

    add(&fixture, "cargo test").await.expect("stored");
    add(&fixture, "cargo test").await.expect("stored again");
    add(&fixture, "git status").await.expect("stored");

    let entries = list(&fixture).await;
    let texts: Vec<&str> = entries.iter().map(|entry| entry.text.as_str()).collect();
    assert_eq!(
        texts,
        ["git status", "cargo test"],
        "newest first, no duplicate"
    );
    assert_eq!(entries[0].source_session_id, SESSION_A);
    assert_eq!(entries[0].source_worker_fp, WORKER_FP);
    assert_eq!(entries[0].source_kind, "selection");
}

#[tokio::test]
async fn an_oversized_copy_or_an_unknown_session_is_refused() {
    let fixture = WorkspacesFixture::new("clipboard-refuse").await;
    fixture.enroll_session(SESSION_A, "/srv/one").await;

    let oversized = "x".repeat(CLIPBOARD_TEXT_MAX_BYTES + 1);
    assert_eq!(
        add(&fixture, &oversized).await,
        Err(ErrorCode::InvalidArgument)
    );
    let unknown = handle_clipboard_add(
        &fixture.core,
        &device_caller(),
        roost_proto::ClipboardAddRequest {
            text: "hi".to_owned(),
            session_id: "99999999-9999-4999-8999-999999999999".to_owned(),
            source_kind: "selection".to_owned(),
            ..Default::default()
        },
    )
    .await
    .map(|_| ())
    .map_err(|error| error.code);
    assert_eq!(unknown, Err(ErrorCode::NotFound));
    assert!(list(&fixture).await.is_empty());
}

#[tokio::test]
async fn the_history_keeps_the_newest_fifty_and_forgets_after_seven_days() {
    let fixture = WorkspacesFixture::new("clipboard-caps").await;
    for index in 0..(CLIPBOARD_HISTORY_LIMIT + 5) {
        capture_osc52(
            &fixture.core,
            SESSION_A,
            WORKER_FP,
            &format!("copy {index}"),
            1_000 + index,
        )
        .await
        .expect("captured");
    }
    let entries = list(&fixture).await;
    assert_eq!(entries.len(), CLIPBOARD_HISTORY_LIMIT as usize);
    assert_eq!(
        entries[0].text,
        format!("copy {}", CLIPBOARD_HISTORY_LIMIT + 4)
    );
    assert_eq!(entries[0].source_kind, "osc52");

    let pruned = prune_clipboard_history(&fixture.database, 1_000 + CLIPBOARD_RETENTION_MS + 30)
        .await
        .expect("pruned");
    assert_eq!(pruned, 25, "rows stamped before the window are gone");
    assert_eq!(list(&fixture).await.len(), 25);
}

#[tokio::test]
async fn a_deleted_entry_is_gone_and_clear_empties_the_history() {
    let fixture = WorkspacesFixture::new("clipboard-delete").await;
    for (index, text) in ["one", "two", "three"].into_iter().enumerate() {
        capture_osc52(
            &fixture.core,
            SESSION_A,
            WORKER_FP,
            text,
            1_000 + index as i64,
        )
        .await
        .expect("captured");
    }
    let doomed = list(&fixture).await[1].id.clone();
    handle_clipboard_delete(
        &fixture.core,
        &device_caller(),
        roost_proto::ClipboardDeleteRequest {
            id: doomed,
            ..Default::default()
        },
    )
    .await
    .expect("deleted");
    let texts: Vec<String> = list(&fixture)
        .await
        .into_iter()
        .map(|entry| entry.text)
        .collect();
    assert_eq!(texts, ["three", "one"]);

    handle_clipboard_clear(
        &fixture.core,
        &device_caller(),
        roost_proto::ClipboardClearRequest::default(),
    )
    .await
    .expect("cleared");
    assert!(list(&fixture).await.is_empty());
}
