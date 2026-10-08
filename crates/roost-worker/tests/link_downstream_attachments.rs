#![cfg(unix)]
//! The coordinator link's attachment arms against the real attachment owner:
//! relayed chunks land one file under v2's name and manifest and are answered
//! `rpc-ok { abs_path }` only once complete, a refusal is v2's `rpc-error`
//! message, a grant is installed and revoked, and a direct status reads the
//! durable operation. Ports the attachment cases of v2
//! `transport/coord-link-downstream.ts`, `coord-link-deps.ts` and `coord-link-direct-terminal.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod attachment_support;
mod link_downstream_support;

use std::sync::Arc;
use std::time::Instant;

use attachment_support::{Scratch, digest};
use link_downstream_support::{FakeLink, Fakes, OwnerMode, next_uplink, settle_tasks};
use roost_proto::{
    DAttachmentChunk, DAttachmentDirectStatusRequest, DLocalAttachmentGrant,
    DLocalAttachmentGrantRevoke,
};
use roost_protocol::wire::coord_worker::{
    CoordWorkerDownstream as Down, CoordWorkerUpstream as Up,
};
use roost_worker::attachments::direct_owners::{AttachmentDirect, AttachmentDirectDeps};
use roost_worker::attachments::file_store::probe_attachment;
use roost_worker::attachments::grants::AttachmentGrantStore;
use roost_worker::attachments::link::AttachmentLink;
use roost_worker::attachments::system_clock;
use roost_worker::attachments::upload::AttachmentOperations;
use roost_worker::peer::CoordinatorGeneration;
use roost_worker::peer::PeerTransportConfig;
use roost_worker::peer::native::str0m_loader;
use roost_worker::runtime::downstream::Dispatcher;
use roost_worker::uplink::{UplinkReceiver, channel};
use serde_json::json;

const EPOCH: &str = "process-epoch-attach";
const SESSION: &str = "00000000-0000-4000-8000-0000000000a7";

struct Harness {
    scratch: Scratch,
    grants: Arc<AttachmentGrantStore>,
    dispatcher: Dispatcher,
    receiver: UplinkReceiver,
}

fn harness(label: &str) -> Harness {
    let scratch = Scratch::new(label);
    let operations = AttachmentOperations::new(scratch.base(), system_clock());
    let grants = Arc::new(AttachmentGrantStore::system(EPOCH));
    let direct = AttachmentDirect::new(AttachmentDirectDeps {
        grants: Arc::clone(&grants),
        operations: operations.clone(),
        worker_fingerprint: "b".repeat(64),
        worker_epoch: EPOCH.to_owned(),
        peer: PeerTransportConfig {
            enabled: false,
            bind_address: None,
            port_range: None,
        },
        native_loader: str0m_loader(),
        coordinator_generation: CoordinatorGeneration::default(),
    });
    let mut owners = Fakes::new(OwnerMode::Answer).owners();
    owners.attachments = Arc::new(AttachmentLink::new(operations, Arc::clone(&grants), direct));
    let (uplink, receiver) = channel();
    Harness {
        scratch,
        grants,
        dispatcher: Dispatcher::new(uplink, EPOCH, Some(owners)),
        receiver,
    }
}

fn relay_chunk(request_id: &str, data: &[u8], seq: u32, last: bool) -> Down {
    Down::AttachmentChunk(DAttachmentChunk {
        request_id: request_id.to_owned(),
        session_id: SESSION.to_owned(),
        filename: "notes (draft).tar.gz".to_owned(),
        short_path: false,
        data: data.to_vec(),
        last,
        seq,
        ..Default::default()
    })
}

impl Harness {
    fn dispatch(&self, frame: Down) -> Vec<Up> {
        let mut link = FakeLink::default();
        self.dispatcher.dispatch(frame, Instant::now(), &mut link);
        link.replies
    }
}

#[tokio::test]
async fn relayed_chunks_land_one_file_under_v2s_name_and_only_the_last_is_answered() {
    let mut harness = harness("link-relay");
    for (seq, part) in [b"alpha-".as_slice(), b"beta-", b"gamma"]
        .iter()
        .enumerate()
    {
        let last = seq == 2;
        assert!(
            harness
                .dispatch(relay_chunk("up-1", part, u32::try_from(seq).unwrap(), last))
                .is_empty()
        );
    }
    let Up::RpcOk {
        request_id, data, ..
    } = next_uplink(&mut harness.receiver).await
    else {
        panic!("a completed relay upload answers rpc-ok");
    };
    assert_eq!(request_id, "up-1");
    let abs_path = data["abs_path"].as_str().unwrap().to_owned();
    assert!(
        abs_path.ends_with(&format!("/{SESSION}/notes (draft).tar.gz")),
        "{abs_path}"
    );
    assert_eq!(std::fs::read(&abs_path).unwrap(), b"alpha-beta-gamma");
    let probe = probe_attachment(
        &harness.scratch.base(),
        SESSION,
        &digest(b"alpha-beta-gamma"),
        false,
    );
    assert_eq!((probe.hit, probe.abs_path), (true, abs_path));

    harness.dispatch(relay_chunk("up-2", b"again", 0, true));
    let Up::RpcOk { data, .. } = next_uplink(&mut harness.receiver).await else {
        panic!("the second upload answers rpc-ok");
    };
    // v2 attachment-file-store.ts:129-132: Node's `extname` of `x.tar.gz` is
    // `.gz`, so the counter lands before the last extension only.
    let second = data["abs_path"].as_str().unwrap();
    assert!(second.ends_with("/notes (draft).tar (2).gz"), "{second}");
}

#[tokio::test]
async fn a_refused_relay_chunk_is_answered_with_v2s_message() {
    let mut harness = harness("link-refused");
    harness.dispatch(relay_chunk("up-3", b"first", 0, false));
    harness.dispatch(relay_chunk("up-3", b"skipped", 2, true));
    let answer = next_uplink(&mut harness.receiver).await;
    assert_eq!(
        answer,
        Up::RpcError {
            request_id: "up-3".to_owned(),
            message: "attachment chunk is out of order".to_owned(),
            trace_id: None
        }
    );
    settle_tasks().await;
    assert!(
        harness.receiver.try_recv().is_none(),
        "the non-final chunk was never answered"
    );
}

#[tokio::test]
async fn a_grant_is_installed_revoked_by_device_and_a_status_reads_the_operation() {
    let mut harness = harness("link-grant");
    let grant = DLocalAttachmentGrant {
        request_id: "g-req".to_owned(),
        grant_id: "grant-1".to_owned(),
        secret_sha256: digest(b"secret"),
        session_id: SESSION.to_owned(),
        upload_id: "up-4".to_owned(),
        filename: "photo.png".to_owned(),
        total_bytes: 4,
        device_fingerprint: "device-1".to_owned(),
        tab_id: "tab-1".to_owned(),
        ttl_ms: 60_000,
        worker_epoch: EPOCH.to_owned(),
        ..Default::default()
    };
    let installed = harness.dispatch(Down::LocalAttachmentGrant(grant.clone()));
    assert_eq!(
        installed,
        [Up::RpcOk {
            request_id: "g-req".to_owned(),
            data: json!({ "grant_id": "grant-1" }),
            trace_id: None
        }]
    );
    let stale_epoch = DLocalAttachmentGrant {
        worker_epoch: "other".to_owned(),
        ..grant
    };
    let refused = harness.dispatch(Down::LocalAttachmentGrant(stale_epoch));
    assert_eq!(
        refused,
        [Up::RpcError {
            request_id: "g-req".to_owned(),
            message: "attachment grant is invalid".to_owned(),
            trace_id: None
        }]
    );

    let revoke = DLocalAttachmentGrantRevoke {
        device_fingerprint: "device-1".to_owned(),
        ..Default::default()
    };
    assert!(
        harness
            .dispatch(Down::LocalAttachmentGrantRevoke(revoke))
            .is_empty()
    );
    assert!(harness.grants.current("grant-1").is_none());

    harness.dispatch(relay_chunk("up-4", b"done", 0, true));
    next_uplink(&mut harness.receiver).await;
    let request = DAttachmentDirectStatusRequest {
        request_id: "s-1".to_owned(),
        session_id: SESSION.to_owned(),
        upload_id: "up-4".to_owned(),
        ..Default::default()
    };
    let [Up::AttachmentDirectStatus(answer)] =
        <[Up; 1]>::try_from(harness.dispatch(Down::AttachmentDirectStatusRequest(request)))
            .unwrap()
    else {
        panic!("one status");
    };
    let status = answer.status.as_option().unwrap();
    assert_eq!(
        (
            answer.request_id.as_str(),
            status.committed,
            status.bytes_received
        ),
        ("s-1", true, 4)
    );
    assert_eq!(status.error, "");
}
