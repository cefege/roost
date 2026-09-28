//! Typed receipt-status correlation: only the exact current generation settles
//! a request, a missing or mismatched receipt is a worker failure, and the
//! deadline is measured on a paused clock rather than a wall-clock sleep.
//! Ports `apps/coord/tests/attachments/attachment-direct-status-results.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod attachments_direct_support;

use std::sync::Arc;

use attachments_direct_support::{SESSION_ID, TestWorker, UPLOAD_ID, WORKER_FP, install_worker};
use connectrpc::{ConnectError, ErrorCode};
use roost_coord::attachments::status_results::AttachmentDirectStatusResults;
use roost_coord::coord_core::worker_handle::{WorkerHandle, WorkerRegistry};
use roost_proto::buffa::MessageField;
use roost_proto::{AttachmentTransferStatus, WAttachmentDirectStatus};
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use tokio::task::JoinHandle;

type StatusTask = JoinHandle<Result<AttachmentTransferStatus, ConnectError>>;

fn status_frame(request_id: &str, upload_id: &str, committed: bool) -> WAttachmentDirectStatus {
    WAttachmentDirectStatus {
        request_id: request_id.to_owned(),
        status: MessageField::some(AttachmentTransferStatus {
            upload_id: upload_id.to_owned(),
            next_seq: 2,
            bytes_received: 1_024,
            last_chunk_sha256: "a".repeat(64),
            committed,
            abs_path: if committed {
                "/attachment/path".to_owned()
            } else {
                String::new()
            },
            error: String::new(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn status_request_id(frame: &CoordWorkerDownstream) -> Option<String> {
    match frame {
        CoordWorkerDownstream::AttachmentDirectStatusRequest(sent) => Some(sent.request_id.clone()),
        _ => None,
    }
}

/// Start one request and return it with the correlation id the worker saw.
async fn request(
    owner: &Arc<AttachmentDirectStatusResults>,
    worker: &TestWorker,
) -> (StatusTask, String) {
    let seen: Vec<String> = worker
        .frames()
        .iter()
        .filter_map(status_request_id)
        .collect();
    let (owner, handle) = (Arc::clone(owner), Arc::clone(&worker.handle));
    let task = tokio::spawn(async move { owner.request(&handle, SESSION_ID, UPLOAD_ID).await });
    let request_id = worker
        .wait_for(|frame| status_request_id(frame).filter(|fresh| !seen.contains(fresh)))
        .await;
    (task, request_id)
}

// v2 "accepts only a current exact worker handle and matching upload receipt".
#[tokio::test]
async fn only_the_exact_generation_and_matching_upload_settle_a_receipt() {
    let workers = Arc::new(WorkerRegistry::new());
    let owner = Arc::new(AttachmentDirectStatusResults::new(Arc::clone(&workers)));
    let worker = install_worker(&workers, WORKER_FP, "status-epoch", &[]);
    let (operation, request_id) = request(&owner, &worker).await;
    let rogue = Arc::new(WorkerHandle::clone(&worker.handle));

    assert!(!owner.accept_status(&rogue, &status_frame(&request_id, UPLOAD_ID, false)));
    assert!(owner.accept_status(
        &worker.handle,
        &status_frame(&request_id, "other-upload", false)
    ));
    assert_eq!(
        operation.await.unwrap().unwrap_err().code,
        ErrorCode::Unavailable
    );

    let (follow_up, follow_up_id) = request(&owner, &worker).await;
    assert!(owner.accept_status(
        &worker.handle,
        &status_frame(&follow_up_id, UPLOAD_ID, true)
    ));
    let status = follow_up.await.unwrap().expect("the committed receipt");
    assert_eq!(status.upload_id, UPLOAD_ID);
    assert_eq!(status.next_seq, 2);
    assert!(status.committed);
    assert_eq!(status.abs_path, "/attachment/path");
}

// v2 "does not accept a stale worker replacement or missing optional status".
#[tokio::test]
async fn a_replaced_generation_and_a_missing_receipt_settle_nothing_as_success() {
    let workers = Arc::new(WorkerRegistry::new());
    let owner = Arc::new(AttachmentDirectStatusResults::new(Arc::clone(&workers)));
    let first = install_worker(&workers, WORKER_FP, "status-epoch", &[]);
    let (original, original_id) = request(&owner, &first).await;
    let replacement = install_worker(&workers, WORKER_FP, "status-replacement", &[]);

    assert!(!owner.accept_status(&first.handle, &status_frame(&original_id, UPLOAD_ID, false)));
    owner.cancel_for_worker_handle(&first.handle, "connection_superseded");
    assert_eq!(
        original.await.unwrap().unwrap_err().code,
        ErrorCode::Unavailable
    );

    let (current, current_id) = request(&owner, &replacement).await;
    let missing = WAttachmentDirectStatus {
        request_id: current_id,
        ..Default::default()
    };
    assert!(owner.accept_status(&replacement.handle, &missing));
    assert_eq!(
        current.await.unwrap().unwrap_err().code,
        ErrorCode::Unavailable
    );
}

// v2 "times out a pending status request deterministically".
#[tokio::test(start_paused = true)]
async fn an_unanswered_request_expires_at_the_status_deadline() {
    let workers = Arc::new(WorkerRegistry::new());
    let owner = Arc::new(AttachmentDirectStatusResults::new(Arc::clone(&workers)));
    let worker = install_worker(&workers, WORKER_FP, "status-epoch", &[]);
    let started = tokio::time::Instant::now();
    let error = owner
        .request(&worker.handle, SESSION_ID, UPLOAD_ID)
        .await
        .unwrap_err();

    assert_eq!(error.code, ErrorCode::DeadlineExceeded);
    assert_eq!(started.elapsed(), std::time::Duration::from_millis(8_000));
    let request_id = match worker.frames().pop() {
        Some(CoordWorkerDownstream::AttachmentDirectStatusRequest(sent)) => sent.request_id,
        other => panic!("expected the status request, saw {other:?}"),
    };
    assert!(
        !owner.accept_status(&worker.handle, &status_frame(&request_id, UPLOAD_ID, false)),
        "an expired request no longer settles"
    );
}
