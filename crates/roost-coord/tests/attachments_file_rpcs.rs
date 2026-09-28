//! The file and attachment RPCs end to end, over a real database and a worker
//! generation whose socket admits frames: v2's refusal table, the per-method
//! deadline, the pending-before-send ordering, and the upload's final-chunk
//! correlation (v2 `apps/coord/src/attachments/handlers-attachments.ts`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "terminal_screen_support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use connectrpc::ErrorCode;
use roost_coord::attachments::rpc_files::{
    handle_attach_file_chunk, handle_files_mkdir, handle_files_read, handle_list_attachments,
};
use roost_coord::coord_core::worker_handle::WorkerHandle;
use roost_proto::{AttachFileChunkRequest, FilesMkdirRequest, FilesReadRequest};
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::control::ClientControlFrame;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use serde_json::json;
use sqlx::AssertSqlSafe;
use support::{Harness, WORKER_FP, frame_of};

const OFFLINE_FP: &str = "cc00000000000000000000000000000000000000000000000000000000000000";
const UNKNOWN_FP: &str = "bb00000000000000000000000000000000000000000000000000000000000000";

type Sent = Arc<Mutex<Vec<CoordWorkerDownstream>>>;

/// Replace the harness's generation with one whose socket admits every frame.
/// With `answer`, the worker's reply settles the frame's correlation entry
/// INSIDE the send — a reply that races ahead of the handler's next line.
fn accepting_worker(
    harness: &Harness,
    generation: &str,
    answer: Option<serde_json::Value>,
) -> Sent {
    let sent: Sent = Arc::default();
    let log = Arc::clone(&sent);
    let pending = Arc::clone(harness.core.services.scrollback.pending());
    let handle = WorkerHandle::new(
        WorkerFp::try_from(WORKER_FP).unwrap(),
        None,
        generation.to_owned(),
        Default::default(),
        Arc::new(move |frame: CoordWorkerDownstream| {
            let correlation = match &frame {
                CoordWorkerDownstream::BrowserCommand { request_id, .. } => Some(request_id),
                CoordWorkerDownstream::AttachmentChunk(chunk) => Some(&chunk.request_id),
                _ => None,
            };
            if let (Some(reply), Some(request_id)) = (&answer, correlation) {
                pending.resolve(request_id, reply.clone(), Some(WORKER_FP));
            }
            log.lock().unwrap().push(frame);
            1
        }),
    );
    assert!(handle.mark_ready(), "the generation crossed its barrier");
    harness.core.services.workers.insert(Arc::new(handle));
    sent
}

fn read_request(worker_fp: &str) -> FilesReadRequest {
    FilesReadRequest {
        worker_fp: worker_fp.to_owned(),
        path: "/tmp/notes.txt".to_owned(),
        ..Default::default()
    }
}

fn chunk(harness: &Harness, last: bool, seq: u32) -> AttachFileChunkRequest {
    AttachFileChunkRequest {
        upload_id: "upload-1".to_owned(),
        session_id: harness.session_id.clone(),
        filename: "photo.png".to_owned(),
        data: vec![7, 8, 9],
        last,
        seq,
        ..Default::default()
    }
}

#[tokio::test]
async fn a_reply_that_races_the_send_still_settles_the_read() {
    let harness = Harness::new("files-race").await;
    let sent = accepting_worker(
        &harness,
        "gen-2",
        Some(json!({ "content_b64": "aGk=", "size": 2 })),
    );
    let served = tokio::time::timeout(
        Duration::from_secs(5),
        handle_files_read(&harness.core, &harness.caller(), read_request(WORKER_FP)),
    )
    .await
    .expect("a reply that beat the handler to its wait must not be lost to the deadline")
    .expect("the read is served")
    .body;
    assert_eq!(served.data, b"hi");
    assert_eq!(served.size, 2);
    let frames = sent.lock().unwrap();
    assert_eq!(frames.len(), 1, "one read is one browser command");
    let (browser_id, frame, viewer_id, _) = frame_of(&frames[0]);
    assert_eq!((browser_id, viewer_id), ("browser-fp", "browser-fp"));
    assert!(
        matches!(frame, ClientControlFrame::ReadFile { path, .. } if path == "/tmp/notes.txt"),
        "a read is a read-file frame naming the path, got {frame:?}"
    );
}

#[tokio::test]
async fn the_worker_lookup_answers_v2s_refusal_table() {
    let harness = Harness::new("files-refusals").await;
    let sent = accepting_worker(&harness, "gen-2", None);
    let db = harness.core.services.db.pool();
    sqlx::query(AssertSqlSafe(
        "INSERT INTO workers (fp, label, os, registered_at_ms, last_seen_ms, dashboard_id) \
         SELECT ?1, 'desk', 'linux', 0, 0, dashboard_id FROM workers WHERE fp = ?2",
    ))
    .bind(OFFLINE_FP)
    .bind(WORKER_FP)
    .execute(db)
    .await
    .expect("a second worker row with no socket");
    let refusal = |fp: &'static str| {
        let core = harness.core.clone();
        let caller = harness.caller();
        async move {
            handle_files_read(&core, &caller, read_request(fp))
                .await
                .err()
                .expect("the read is refused")
        }
    };

    let unknown = refusal(UNKNOWN_FP).await;
    assert_eq!(
        unknown.code,
        ErrorCode::NotFound,
        "no row: the machine is gone"
    );
    assert_eq!(unknown.message.as_deref(), Some("worker not found"));
    let offline = refusal(OFFLINE_FP).await;
    assert_eq!(
        offline.code,
        ErrorCode::FailedPrecondition,
        "a row with no socket: the link is down, retry"
    );
    assert_eq!(offline.message.as_deref(), Some("worker not connected"));
    sqlx::query("UPDATE workers SET deleted_at_ms = 1 WHERE fp = ?1")
        .bind(OFFLINE_FP)
        .execute(db)
        .await
        .expect("tombstone the offline worker");
    assert_eq!(refusal(OFFLINE_FP).await.code, ErrorCode::NotFound);
    assert!(sent.lock().unwrap().is_empty(), "a refusal sends nothing");

    let listing = handle_list_attachments(
        &harness.core,
        &harness.caller(),
        roost_proto::ListAttachmentsRequest {
            session_id: "not-a-session".to_owned(),
            ..Default::default()
        },
    )
    .await
    .err()
    .expect("an id that is not a session is refused");
    assert_eq!(
        (listing.code, listing.message.as_deref()),
        (ErrorCode::NotFound, Some("session not found")),
        "v2 looks the id up rather than parsing it"
    );
}

#[tokio::test]
async fn a_worker_that_never_replies_is_refused_at_the_v2_deadline() {
    let harness = Harness::new("files-deadline").await;
    let sent = accepting_worker(&harness, "gen-2", None);
    let core = harness.core.clone();
    let caller = harness.caller();
    let task = tokio::spawn(async move {
        let request = FilesMkdirRequest {
            worker_fp: WORKER_FP.to_owned(),
            path: "/tmp/new".to_owned(),
            ..Default::default()
        };
        handle_files_mkdir(&core, &caller, request).await
    });
    for _ in 0..2_000 {
        if !sent.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert_eq!(
        sent.lock().unwrap().len(),
        1,
        "the mkdir reached the worker"
    );
    // The database work is behind us; from here the clock only moves when
    // every task is waiting, so ten seconds pass without a real wait.
    tokio::time::pause();
    let outcome = tokio::time::timeout(Duration::from_secs(60), task)
        .await
        .expect("the relay owns a deadline; without one an unanswered call waits forever")
        .expect("the handler task finished");
    let error = outcome.err().expect("an unanswered mkdir is refused");
    assert_eq!(error.code, ErrorCode::DeadlineExceeded);
    assert_eq!(
        error.message.as_deref(),
        Some("worker did not reply within 10000ms")
    );
    assert_eq!(
        harness.core.services.scrollback.pending().pending_count(),
        0,
        "the timed-out entry leaves the table"
    );
}

#[tokio::test]
async fn only_the_final_upload_chunk_waits_for_the_worker() {
    let harness = Harness::new("files-upload").await;
    let sent = accepting_worker(&harness, "gen-2", None);
    let first =
        handle_attach_file_chunk(&harness.core, &harness.caller(), chunk(&harness, false, 0))
            .await
            .expect("a non-final chunk is accepted")
            .body;
    assert_eq!(
        first.abs_path, "",
        "a non-final chunk answers an empty path"
    );
    assert_eq!(
        harness.core.services.scrollback.pending().pending_count(),
        0,
        "a non-final chunk registers no correlation entry"
    );
    match &sent.lock().unwrap()[..] {
        [CoordWorkerDownstream::AttachmentChunk(sent_chunk)] => {
            assert_eq!(sent_chunk.request_id, "upload-1");
            assert_eq!(sent_chunk.session_id, harness.session_id);
            assert_eq!(
                (sent_chunk.data.as_slice(), sent_chunk.last),
                (&[7u8, 8, 9][..], false)
            );
        }
        other => panic!("one attachment-chunk frame, got {other:?}"),
    }

    accepting_worker(
        &harness,
        "gen-3",
        Some(json!({ "abs_path": "/att/photo.png" })),
    );
    let last = tokio::time::timeout(
        Duration::from_secs(5),
        handle_attach_file_chunk(&harness.core, &harness.caller(), chunk(&harness, true, 1)),
    )
    .await
    .expect("the final chunk's entry exists before the chunk is sent")
    .expect("the final chunk is stored")
    .body;
    assert_eq!(last.abs_path, "/att/photo.png");

    let mut unnamed = chunk(&harness, true, 2);
    unnamed.upload_id.clear();
    let refused = handle_attach_file_chunk(&harness.core, &harness.caller(), unnamed)
        .await
        .err()
        .expect("an upload with no id is refused");
    assert_eq!(
        (refused.code, refused.message.as_deref()),
        (ErrorCode::InvalidArgument, Some("upload_id required"))
    );
    let mut sessionless = chunk(&harness, false, 3);
    sessionless.session_id.clear();
    let refused = handle_attach_file_chunk(&harness.core, &harness.caller(), sessionless)
        .await
        .err()
        .expect("an upload with no session is refused");
    assert_eq!(
        (refused.code, refused.message.as_deref()),
        (ErrorCode::InvalidArgument, Some("session_id required"))
    );
}
