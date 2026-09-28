//! Shared fixtures for the direct-attachment tests: fake worker generations
//! that record their downstream frames, an in-memory grant port, the v2 SDP
//! fixture, and a booted coordinator with one open session. Shared by the
//! `attachments_*` binaries; ports `apps/coord/tests/attachments/
//! attachment-peer-negotiation-test-fixture.ts`.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use roost_coord::attachments::grant_state::{
    AttachmentGrantDescriptor, AttachmentGrantInvalidation, AttachmentGrantInvalidationListener,
    AttachmentGrantLease, AttachmentGrantPort,
};
use roost_coord::attachments::peer_state::{AttachmentPeerCaller, AttachmentPeerSignalConfig};
use roost_coord::auth::principal::Principal;
use roost_coord::coord_core::CoordCore;
use roost_coord::coord_core::boot_facts::BootFacts;
use roost_coord::coord_core::caller::{Caller, ListenerTrust};
use roost_coord::coord_core::worker_handle::{WorkerHandle, WorkerRegistry};
use roost_coord::services::CoordServices;
use roost_proto::{SessionsNegotiateAttachmentPeerRequest, WLocalAttachmentPeerAnswer};
use roost_protocol::versioning::CAPABILITY_ATTACHMENT_TRANSFER_PEER_WEBRTC_V1;
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use sqlx::AssertSqlSafe;

pub const WORKER_FP: &str = "aa00000000000000000000000000000000000000000000000000000000000000";
pub const OTHER_WORKER_FP: &str =
    "bb00000000000000000000000000000000000000000000000000000000000000";
pub const DEVICE_FP: &str = "dd00000000000000000000000000000000000000000000000000000000000000";
pub const ACCOUNT_ID: &str = "test-account";
pub const TAB_ID: &str = "attachment-tab";
pub const GRANT_ID: &str = "attachment-peer-test-grant";
pub const SESSION_ID: &str = "00000000-0000-4000-8000-000000000001";
pub const UPLOAD_ID: &str = "00000000-0000-4000-8000-000000000002";
pub const PEER_ID: &str = "00000000-0000-4000-8000-000000000010";
pub const EPOCH: &str = "attachment-epoch";

/// The owner key `Principal::capture_owner_key` derives for the test device.
pub fn owner_key() -> String {
    format!("account-device:{ACCOUNT_ID}:{DEVICE_FP}")
}

/// v2's fixture offer/answer: a data-channel-only SDP the inspector admits.
pub fn valid_sdp() -> String {
    let fingerprint: Vec<String> = (0..32).map(|index| format!("{index:02x}")).collect();
    [
        "v=0".to_owned(),
        "o=- 1 2 IN IP4 127.0.0.1".to_owned(),
        "s=-".to_owned(),
        "t=0 0".to_owned(),
        "m=application 9 UDP/DTLS/SCTP webrtc-datachannel".to_owned(),
        "a=setup:actpass".to_owned(),
        format!("a=fingerprint:sha-256 {}", fingerprint.join(":")),
        "a=ice-ufrag:attachment-offer".to_owned(),
        format!("a=ice-pwd:{}", "p".repeat(22)),
        "a=max-message-size:16384".to_owned(),
        "a=candidate:host 1 udp 2122260223 192.0.2.8 5000 typ host".to_owned(),
        String::new(),
    ]
    .join("\r\n")
}

/// Every downstream frame a fake generation was handed.
pub type SentFrames = Arc<Mutex<Vec<CoordWorkerDownstream>>>;

/// One fake, ready worker generation.
pub struct TestWorker {
    pub handle: Arc<WorkerHandle>,
    pub sent: SentFrames,
}

impl TestWorker {
    pub fn frames(&self) -> Vec<CoordWorkerDownstream> {
        self.sent.lock().unwrap().clone()
    }

    /// Wait until a frame `pick` recognises was sent, and return it.
    pub async fn wait_for<T>(&self, pick: impl Fn(&CoordWorkerDownstream) -> Option<T>) -> T {
        for _ in 0..2_000 {
            if let Some(found) = self.frames().iter().rev().find_map(&pick) {
                return found;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!("the expected frame was never sent; saw {:?}", self.frames());
    }
}

static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Make a ready generation current for `worker_fp`, fencing any predecessor.
pub fn install_worker(
    workers: &WorkerRegistry,
    worker_fp: &str,
    epoch: &str,
    capabilities: &[&str],
) -> TestWorker {
    let sent = SentFrames::default();
    let log = Arc::clone(&sent);
    let generation = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let handle = WorkerHandle::new(
        WorkerFp::try_from(worker_fp).unwrap(),
        Some(epoch.to_owned()),
        format!(
            "attachment-connection-{}-{epoch}-{generation}",
            &worker_fp[..8]
        ),
        capabilities
            .iter()
            .map(|capability| (*capability).to_owned())
            .collect::<BTreeSet<_>>(),
        Arc::new(move |frame: CoordWorkerDownstream| {
            log.lock().unwrap().push(frame);
            1
        }),
    );
    handle.mark_ready();
    let handle = Arc::new(handle);
    workers.insert(Arc::clone(&handle));
    TestWorker { handle, sent }
}

/// A peer-capable generation.
pub fn install_peer_worker(workers: &WorkerRegistry, worker_fp: &str, epoch: &str) -> TestWorker {
    install_worker(
        workers,
        worker_fp,
        epoch,
        &[CAPABILITY_ATTACHMENT_TRANSFER_PEER_WEBRTC_V1],
    )
}

/// An in-memory grant authority, as v2's `TestAttachmentGrants`.
#[derive(Default)]
pub struct TestAttachmentGrants {
    leases: Mutex<HashMap<String, AttachmentGrantLease>>,
    listeners: Mutex<Vec<AttachmentGrantInvalidationListener>>,
}

impl TestAttachmentGrants {
    pub fn install(&self, lease: AttachmentGrantLease) {
        self.leases
            .lock()
            .unwrap()
            .insert(lease.grant_id.clone(), lease);
    }

    pub fn invalidate(&self, invalidation: &AttachmentGrantInvalidation) {
        for listener in self.listeners.lock().unwrap().iter() {
            listener(invalidation);
        }
    }
}

impl AttachmentGrantPort for TestAttachmentGrants {
    fn owned_grant(
        &self,
        owner_key: &str,
        tab_id: &str,
        worker_fp: &str,
        grant_id: &str,
    ) -> Option<AttachmentGrantLease> {
        let lease = self.leases.lock().unwrap().get(grant_id)?.clone();
        (lease.owner_key == owner_key && lease.tab_id == tab_id && lease.worker_fp == worker_fp)
            .then_some(lease)
    }

    fn subscribe_invalidation(&self, listener: AttachmentGrantInvalidationListener) {
        self.listeners.lock().unwrap().push(listener);
    }
}

/// The one upload every test grants.
pub fn descriptor(upload_id: &str) -> AttachmentGrantDescriptor {
    AttachmentGrantDescriptor {
        session_id: SESSION_ID.to_owned(),
        upload_id: upload_id.to_owned(),
        filename: "attachment.bin".to_owned(),
        short_path: false,
        total_bytes: 1_024,
    }
}

/// A lease for `worker`, owned by the test device and tab.
pub fn lease_for(worker: &Arc<WorkerHandle>) -> AttachmentGrantLease {
    AttachmentGrantLease {
        grant_id: GRANT_ID.to_owned(),
        owner_key: owner_key(),
        device_fingerprint: DEVICE_FP.to_owned(),
        tab_id: TAB_ID.to_owned(),
        worker_fp: worker.worker_fp.as_str().to_owned(),
        worker_epoch: worker.process_epoch.clone().unwrap_or_default(),
        descriptor: descriptor(UPLOAD_ID),
        expires_at_ms: i64::MAX,
        worker_handle: Arc::clone(worker),
    }
}

/// The test device, authenticated on the test tab.
pub fn peer_caller() -> AttachmentPeerCaller {
    AttachmentPeerCaller {
        owner_key: owner_key(),
        device_fingerprint: DEVICE_FP.to_owned(),
        tab_id: Some(TAB_ID.to_owned()),
    }
}

pub fn peers_enabled() -> AttachmentPeerSignalConfig {
    AttachmentPeerSignalConfig {
        enabled: true,
        stun_urls: Vec::new(),
    }
}

/// A well-formed negotiation for `worker`'s current epoch.
pub fn peer_request(
    worker: &WorkerHandle,
    peer_id: &str,
) -> SessionsNegotiateAttachmentPeerRequest {
    SessionsNegotiateAttachmentPeerRequest {
        worker_fp: worker.worker_fp.as_str().to_owned(),
        grant_id: GRANT_ID.to_owned(),
        tab_id: TAB_ID.to_owned(),
        peer_id: peer_id.to_owned(),
        offer_sdp: valid_sdp(),
        worker_epoch: worker.process_epoch.clone().unwrap_or_default(),
        ..Default::default()
    }
}

/// The worker's answer to `offer`, from `worker`'s generation.
pub fn peer_answer(
    request_id: &str,
    peer_id: &str,
    worker: &WorkerHandle,
) -> WLocalAttachmentPeerAnswer {
    WLocalAttachmentPeerAnswer {
        request_id: request_id.to_owned(),
        connection_generation: worker.connection_generation.clone(),
        worker_epoch: worker.process_epoch.clone().unwrap_or_default(),
        peer_id: peer_id.to_owned(),
        answer_sdp: valid_sdp(),
        ..Default::default()
    }
}

/// The last offer a generation was sent: (request id, peer id, frame).
pub fn offer_of(frame: &CoordWorkerDownstream) -> Option<roost_proto::DLocalAttachmentPeerOffer> {
    match frame {
        CoordWorkerDownstream::LocalAttachmentPeerOffer(offer) => Some(offer.clone()),
        _ => None,
    }
}

/// The browser caller the RPC handlers see.
pub fn browser_caller(tab_id: Option<&str>) -> Caller {
    Caller {
        principal: Principal::AccountDevice {
            fingerprint: DEVICE_FP.to_owned(),
            label: "Attachment browser".to_owned(),
            account_id: ACCOUNT_ID.to_owned(),
        },
        tab_id: tab_id.map(str::to_owned),
        remote_address: Some("127.0.0.1:51000".to_owned()),
        on_host: true,
        listener_trust: ListenerTrust::DirectLoopback,
    }
}

/// A booted coordinator over a scratch database with one open session on
/// `WORKER_FP`, terminal peers enabled with one STUN server.
pub struct DirectHarness {
    pub core: CoordCore,
    root: PathBuf,
}

impl DirectHarness {
    pub async fn new(label: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("roost-attachments-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let database = roost_coord::db::open(&root.join("coord.db"))
            .await
            .expect("a migrated database");
        let tenant =
            roost_coord::auth::self_hosted_tenant::ensure_self_hosted_tenant(&database, 1_000)
                .await
                .expect("the self-hosted tenant");
        for statement in [
            format!(
                "INSERT INTO workers (fp, label, os, registered_at_ms, last_seen_ms, dashboard_id) \
                 VALUES ('{WORKER_FP}', 'laptop', 'linux', 0, 0, '{}')",
                tenant.dashboard_id
            ),
            format!(
                "INSERT INTO sessions (id, dashboard_id, worker_fp, channel, kind, cwd, status, created_at) \
                 VALUES ('{SESSION_ID}', '{}', '{WORKER_FP}', 1, 'shell', '/tmp', 'open', 0)",
                tenant.dashboard_id
            ),
        ] {
            sqlx::query(AssertSqlSafe(statement))
                .execute(database.pool())
                .await
                .expect("a fixture row");
        }
        let config = roost_host::CoordConfig::parse(roost_host::CoordConfigInput {
            db_path: Some(root.join("coord.db")),
            authorized_keys_path: Some(root.join("authorized_keys")),
            log_dir: Some(root.join("logs")),
            terminal_peer_enabled: Some(true),
            terminal_peer_stun_urls: Some(vec!["stun:stun.example.test:3478".to_owned()]),
            ..Default::default()
        })
        .expect("a coordinator config");
        let services = CoordServices::booted(
            database,
            BootFacts {
                tenant: Some(tenant),
                config: Some(Arc::new(config)),
                process_epoch: "coord-epoch".to_owned(),
                boot_ms: 0,
            },
        );
        Self {
            core: CoordCore::new(Arc::new(services)),
            root,
        }
    }

    pub fn services(&self) -> &Arc<CoordServices> {
        &self.core.services
    }

    /// A ready, peer-capable generation at `EPOCH` that acknowledges every
    /// grant install through the coordinator's own pending-request table.
    pub fn attach_acking_worker(&self) -> (Arc<WorkerHandle>, SentFrames) {
        let sent = SentFrames::default();
        let log = Arc::clone(&sent);
        let pending = Arc::clone(self.services().scrollback.pending());
        let handle = WorkerHandle::new(
            WorkerFp::try_from(WORKER_FP).unwrap(),
            Some(EPOCH.to_owned()),
            "attachment-handler-worker".to_owned(),
            BTreeSet::from([CAPABILITY_ATTACHMENT_TRANSFER_PEER_WEBRTC_V1.to_owned()]),
            Arc::new(move |frame: CoordWorkerDownstream| {
                if let CoordWorkerDownstream::LocalAttachmentGrant(grant) = &frame {
                    pending.resolve(&grant.request_id, serde_json::json!({}), Some(WORKER_FP));
                }
                log.lock().unwrap().push(frame);
                1
            }),
        );
        handle.mark_ready();
        let handle = Arc::new(handle);
        self.services().workers.insert(Arc::clone(&handle));
        (handle, sent)
    }
}

/// Wait until a frame `pick` recognises was sent, and return it.
pub async fn wait_for_frame<T>(
    sent: &SentFrames,
    pick: impl Fn(&CoordWorkerDownstream) -> Option<T>,
) -> T {
    for _ in 0..2_000 {
        if let Some(found) = sent.lock().unwrap().iter().rev().find_map(&pick) {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("the expected frame was never sent");
}

impl Drop for DirectHarness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
