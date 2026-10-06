//! The separate attachment direct-grant registry: digest-only installation
//! acknowledged through the real pending-request table, exact generation
//! fencing, and expiry, device revocation and worker retirement.
//! Ports `apps/coord/tests/attachments/attachment-grant-owner.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod attachments_direct_support;
mod db_support;

use std::sync::{Arc, Mutex};

use attachments_direct_support::{
    DEVICE_FP, OTHER_WORKER_FP, SESSION_ID, TAB_ID, TestWorker, UPLOAD_ID, WORKER_FP, descriptor,
    install_worker, owner_key,
};
use connectrpc::ErrorCode;
use roost_coord::attachments::grant::AttachmentGrantOwner;
use roost_coord::attachments::grant_state::{
    AttachmentGrantInvalidationKind, AttachmentGrantPort, AttachmentGrantRequest,
    AttachmentGrantResult, AttachmentGrantRetireReason,
};
use roost_coord::coord_core::worker_handle::WorkerRegistry;
use roost_coord::terminal_screen::pending_rpcs::PendingRpcs;
use roost_proto::DLocalAttachmentGrant;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use sha2::Digest;
use tokio::task::JoinHandle;

struct GrantFixture {
    workers: Arc<WorkerRegistry>,
    pending: Arc<PendingRpcs>,
    owner: Arc<AttachmentGrantOwner>,
    worker: TestWorker,
    other: TestWorker,
}

impl GrantFixture {
    fn new() -> Self {
        let workers = Arc::new(WorkerRegistry::new());
        let pending = Arc::new(PendingRpcs::new());
        let owner = AttachmentGrantOwner::new(Arc::clone(&workers), Arc::clone(&pending));
        let worker = install_worker(&workers, WORKER_FP, "attachment-epoch", &[]);
        let other = install_worker(&workers, OTHER_WORKER_FP, "other-epoch", &[]);
        Self {
            workers,
            pending,
            owner,
            worker,
            other,
        }
    }

    /// Start one grant and return it with the install frame the worker saw,
    /// proving the grant had not completed before that frame was acknowledged.
    async fn start(
        &self,
        upload_id: &str,
    ) -> (
        JoinHandle<Result<AttachmentGrantResult, connectrpc::ConnectError>>,
        DLocalAttachmentGrant,
    ) {
        let already = self.installs().len();
        let owner = Arc::clone(&self.owner);
        let request = AttachmentGrantRequest {
            owner_key: owner_key(),
            device_fingerprint: DEVICE_FP.to_owned(),
            tab_id: TAB_ID.to_owned(),
            worker_fp: WORKER_FP.to_owned(),
            descriptor: roost_coord::attachments::grant_state::AttachmentGrantDescriptor {
                filename: "diagram.png".to_owned(),
                ..descriptor(upload_id)
            },
        };
        let task = tokio::spawn(async move { owner.grant(request, || async { Ok(()) }).await });
        for _ in 0..2_000 {
            if self.installs().len() > already {
                break;
            }
            assert!(
                !task.is_finished(),
                "the grant completed before its worker ACK was observed"
            );
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        let frame = self.installs().pop().expect("the grant install was sent");
        (task, frame)
    }

    async fn grant_with_ack(&self, upload_id: &str) -> AttachmentGrantResult {
        let (task, frame) = self.start(upload_id).await;
        self.acknowledge(&frame);
        task.await.unwrap().expect("an acknowledged grant")
    }

    fn acknowledge(&self, frame: &DLocalAttachmentGrant) {
        assert!(
            self.pending
                .resolve(&frame.request_id, serde_json::json!({}), Some(WORKER_FP))
        );
    }

    fn installs(&self) -> Vec<DLocalAttachmentGrant> {
        self.worker
            .frames()
            .into_iter()
            .filter_map(|frame| match frame {
                CoordWorkerDownstream::LocalAttachmentGrant(grant) => Some(grant),
                _ => None,
            })
            .collect()
    }

    fn revokes(&self) -> Vec<(String, String)> {
        [(WORKER_FP, &self.worker), (OTHER_WORKER_FP, &self.other)]
            .into_iter()
            .flat_map(|(worker_fp, worker)| {
                worker
                    .frames()
                    .into_iter()
                    .filter_map(move |frame| match frame {
                        CoordWorkerDownstream::LocalAttachmentGrantRevoke(revoke) => {
                            Some((worker_fp.to_owned(), revoke.device_fingerprint))
                        }
                        _ => None,
                    })
            })
            .collect()
    }
}

// v2 "returns a secret only after digest-only worker ACK and keeps the descriptor immutable".
#[tokio::test]
async fn a_secret_returns_only_after_the_digest_only_install_is_acknowledged() {
    let fixture = GrantFixture::new();
    let (task, frame) = fixture.start(UPLOAD_ID).await;
    assert_eq!(frame.session_id, SESSION_ID);
    assert_eq!(frame.upload_id, UPLOAD_ID);
    assert_eq!(frame.filename, "diagram.png");
    assert_eq!(frame.total_bytes, 1_024);
    assert_eq!(frame.device_fingerprint, DEVICE_FP);
    assert_eq!(frame.tab_id, TAB_ID);
    assert_eq!(frame.worker_epoch, "attachment-epoch");
    fixture.acknowledge(&frame);
    let result = task.await.unwrap().expect("an acknowledged grant");

    assert_eq!(result.secret.len(), 64);
    assert!(result.secret.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_ne!(frame.secret_sha256, result.secret);
    assert_eq!(
        frame.secret_sha256,
        hex::encode(sha2::Sha256::digest(result.secret.as_bytes()))
    );
    let owned = fixture
        .owner
        .owned_grant(&owner_key(), TAB_ID, WORKER_FP, &result.lease.grant_id)
        .expect("the lease");
    assert_eq!(owned.descriptor.upload_id, UPLOAD_ID);
    assert_eq!(owned.descriptor.filename, "diagram.png");
    assert_eq!(owned.descriptor.total_bytes, 1_024);
    let other_owner = format!("{}:other", owner_key());
    assert!(
        fixture
            .owner
            .owned_grant(&other_owner, TAB_ID, WORKER_FP, &result.lease.grant_id)
            .is_none()
    );
    assert!(
        fixture
            .owner
            .owned_grant(&owner_key(), "other-tab", WORKER_FP, &result.lease.grant_id)
            .is_none()
    );
}

// v2 "rejects an ACK after handle replacement and never rebinds that attachment grant".
#[tokio::test]
async fn an_ack_after_the_generation_was_replaced_never_binds_the_grant() {
    let fixture = GrantFixture::new();
    let (task, frame) = fixture.start(UPLOAD_ID).await;
    let _replacement = install_worker(
        &fixture.workers,
        WORKER_FP,
        "attachment-epoch-replaced",
        &[],
    );
    fixture.acknowledge(&frame);
    let error = task
        .await
        .unwrap()
        .expect_err("a replaced generation's ACK is refused");

    assert_eq!(error.code, ErrorCode::Unavailable);
    assert!(
        fixture
            .owner
            .owned_grant(&owner_key(), TAB_ID, WORKER_FP, &frame.grant_id)
            .is_none()
    );
}

// v2 "expires, revokes, and retires only separate attachment grant leases".
#[tokio::test]
async fn expiry_device_revocation_and_worker_retirement_each_drop_their_leases() {
    let fixture = GrantFixture::new();
    let first = fixture.grant_with_ack(UPLOAD_ID).await;
    let kinds = Arc::new(Mutex::new(Vec::new()));
    let heard = Arc::clone(&kinds);
    fixture.owner.subscribe_invalidation(Box::new(move |event| {
        heard.lock().unwrap().push(event.kind)
    }));
    fixture.owner.sweep(first.lease.expires_at_ms);
    assert!(
        fixture
            .owner
            .owned_grant(&owner_key(), TAB_ID, WORKER_FP, &first.lease.grant_id)
            .is_none()
    );

    let second = fixture
        .grant_with_ack("00000000-0000-4000-8000-000000000003")
        .await;
    fixture.owner.revoke_device(DEVICE_FP);
    assert!(
        fixture
            .owner
            .owned_grant(&owner_key(), TAB_ID, WORKER_FP, &second.lease.grant_id)
            .is_none()
    );
    assert_eq!(
        fixture.revokes(),
        vec![
            (WORKER_FP.to_owned(), DEVICE_FP.to_owned()),
            (OTHER_WORKER_FP.to_owned(), DEVICE_FP.to_owned())
        ]
    );

    let third = fixture
        .grant_with_ack("00000000-0000-4000-8000-000000000004")
        .await;
    fixture
        .owner
        .retire_worker(WORKER_FP, AttachmentGrantRetireReason::WorkerDeleted);
    assert!(
        fixture
            .owner
            .owned_grant(&owner_key(), TAB_ID, WORKER_FP, &third.lease.grant_id)
            .is_none()
    );
    assert_eq!(
        *kinds.lock().unwrap(),
        vec![
            AttachmentGrantInvalidationKind::GrantExpired,
            AttachmentGrantInvalidationKind::DeviceRevoked,
            AttachmentGrantInvalidationKind::WorkerRetired,
        ]
    );
}

// v2 `grant()` capacity rows: a browser document holds at most
// ATTACHMENT_TRANSFER_MAX_ACTIVE_PER_BROWSER_DOCUMENT live grants, and the
// refusal is decided before anything reaches the worker.
#[tokio::test]
async fn a_document_past_its_grant_capacity_is_refused_before_the_worker_hears() {
    let fixture = GrantFixture::new();
    for index in 0..8 {
        fixture
            .grant_with_ack(&format!("00000000-0000-4000-8000-0000000001{index:02}"))
            .await;
    }
    let request = AttachmentGrantRequest {
        owner_key: owner_key(),
        device_fingerprint: DEVICE_FP.to_owned(),
        tab_id: TAB_ID.to_owned(),
        worker_fp: WORKER_FP.to_owned(),
        descriptor: descriptor("00000000-0000-4000-8000-000000000199"),
    };
    let error = fixture
        .owner
        .grant(request, || async { Ok(()) })
        .await
        .unwrap_err();

    assert_eq!(error.code, ErrorCode::ResourceExhausted);
    assert_eq!(
        error.message.as_deref(),
        Some("attachment grant document capacity is exhausted")
    );
    assert_eq!(
        fixture.installs().len(),
        8,
        "the refused grant was never installed"
    );
}
