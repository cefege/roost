#![cfg(unix)]
//! The direct attachment grant store and hello admission: what an install
//! must carry, what a hello must prove, how expiry, replacement and device
//! revocation are announced, and the peer tuple a WebRTC hello must repeat.
//! Pins v2 `apps/worker/src/attachments/attachment-{grants,transfer-admission}.ts`
//! (v2 covers them only through its direct-socket suite).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod attachment_support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use attachment_support::{TestClock, digest};
use roost_proto::{AttachmentTransferHello, DLocalAttachmentGrant};
use roost_protocol::attachment_transfer::{GRANT_TTL_MS, TransferErrorReason};
use roost_worker::attachments::grants::{
    AttachmentGrantStore, GrantChange, GrantRemovalReason, PeerGrantAuthorization, PeerGrantRequest,
};
use roost_worker::attachments::transfer_admission::{
    AttachmentPeerExpectedTuple, admit_attachment_transfer_hello,
};

const EPOCH: &str = "worker-epoch-1";
const SECRET: &str = "grant-secret";

fn grant(grant_id: &str, device: &str) -> DLocalAttachmentGrant {
    DLocalAttachmentGrant {
        request_id: format!("req-{grant_id}"),
        grant_id: grant_id.to_owned(),
        secret_sha256: digest(SECRET.as_bytes()),
        session_id: "session-1".to_owned(),
        upload_id: format!("upload-{grant_id}"),
        filename: "photo.png".to_owned(),
        short_path: false,
        total_bytes: 1_000,
        device_fingerprint: device.to_owned(),
        tab_id: "tab-1".to_owned(),
        ttl_ms: 10_000,
        worker_epoch: EPOCH.to_owned(),
        ..Default::default()
    }
}

fn hello(frame: &DLocalAttachmentGrant) -> AttachmentTransferHello {
    AttachmentTransferHello {
        grant_id: frame.grant_id.clone(),
        secret: SECRET.to_owned(),
        tab_id: frame.tab_id.clone(),
        device_fingerprint: frame.device_fingerprint.clone(),
        session_id: frame.session_id.clone(),
        upload_id: frame.upload_id.clone(),
        filename: frame.filename.clone(),
        short_path: frame.short_path,
        total_bytes: frame.total_bytes,
        peer_id: String::new(),
        worker_epoch: frame.worker_epoch.clone(),
        ..Default::default()
    }
}

fn recording(store: &AttachmentGrantStore) -> Arc<Mutex<Vec<GrantChange>>> {
    let changes = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&changes);
    let _subscription = store.subscribe(Box::new(move |change| {
        sink.lock().unwrap().push(change.clone())
    }));
    changes
}

#[test]
fn an_install_must_name_this_epoch_a_bounded_ttl_and_a_real_digest() {
    let store = AttachmentGrantStore::new(EPOCH, TestClock::new().clock());
    let refused = [
        DLocalAttachmentGrant {
            worker_epoch: "other-epoch".to_owned(),
            ..grant("g1", "dev")
        },
        DLocalAttachmentGrant {
            ttl_ms: 0,
            ..grant("g1", "dev")
        },
        DLocalAttachmentGrant {
            ttl_ms: u32::try_from(GRANT_TTL_MS).unwrap() + 1,
            ..grant("g1", "dev")
        },
        DLocalAttachmentGrant {
            secret_sha256: "ABC".to_owned(),
            ..grant("g1", "dev")
        },
        DLocalAttachmentGrant {
            upload_id: "a/b".to_owned(),
            ..grant("g1", "dev")
        },
    ];
    for frame in refused {
        assert_eq!(
            store.install(&frame),
            Err("attachment grant is invalid".to_owned())
        );
    }
    assert!(store.install(&grant("g1", "dev")).is_ok());
}

#[test]
fn the_store_holds_at_most_256_grants_but_a_renewal_always_fits() {
    let store = AttachmentGrantStore::new(EPOCH, TestClock::new().clock());
    for index in 0..256 {
        store.install(&grant(&format!("g{index}"), "dev")).unwrap();
    }
    assert_eq!(
        store.install(&grant("one-more", "dev")),
        Err("attachment grant capacity is full".to_owned())
    );
    assert!(
        store.install(&grant("g7", "dev")).is_ok(),
        "renewing an installed grant takes no new slot"
    );
}

#[test]
fn a_hello_is_admitted_only_with_the_exact_descriptor_and_secret() {
    let store = AttachmentGrantStore::new(EPOCH, TestClock::new().clock());
    let frame = grant("g1", "dev");
    store.install(&frame).unwrap();
    let admitted = admit_attachment_transfer_hello(&hello(&frame), &store, None).unwrap();
    assert_eq!(
        (admitted.upload_id.as_str(), admitted.total_bytes),
        ("upload-g1", 1_000)
    );

    let wrong_secret = AttachmentTransferHello {
        secret: "guess".to_owned(),
        ..hello(&frame)
    };
    let wrong_size = AttachmentTransferHello {
        total_bytes: 999,
        ..hello(&frame)
    };
    let named_peer = AttachmentTransferHello {
        peer_id: "peer-1".to_owned(),
        ..hello(&frame)
    };
    for refused in [wrong_secret, wrong_size, named_peer] {
        assert_eq!(
            admit_attachment_transfer_hello(&refused, &store, None),
            Err(TransferErrorReason::GrantUnavailable)
        );
    }
}

#[test]
fn a_peer_port_hello_must_repeat_the_negotiated_tuple() {
    let store = AttachmentGrantStore::new(EPOCH, TestClock::new().clock());
    let frame = grant("g1", "dev");
    store.install(&frame).unwrap();
    let expected = AttachmentPeerExpectedTuple {
        peer_id: "peer-1".to_owned(),
        grant_id: "g1".to_owned(),
        device_fingerprint: "dev".to_owned(),
        tab_id: "tab-1".to_owned(),
        worker_epoch: EPOCH.to_owned(),
    };
    let from_peer = AttachmentTransferHello {
        peer_id: "peer-1".to_owned(),
        ..hello(&frame)
    };
    assert!(admit_attachment_transfer_hello(&from_peer, &store, Some(&expected)).is_ok());
    let other_peer = AttachmentTransferHello {
        peer_id: "peer-2".to_owned(),
        ..hello(&frame)
    };
    assert!(admit_attachment_transfer_hello(&other_peer, &store, Some(&expected)).is_err());
}

#[test]
fn expiry_refuses_a_new_hello_and_is_announced_once() {
    let clock = TestClock::new();
    let store = AttachmentGrantStore::new(EPOCH, clock.clock());
    let changes = recording(&store);
    let frame = grant("g1", "dev");
    store.install(&frame).unwrap();
    let request = PeerGrantRequest {
        grant_id: "g1".to_owned(),
        device_fingerprint: "dev".to_owned(),
        tab_id: "tab-1".to_owned(),
        worker_epoch: EPOCH.to_owned(),
    };
    assert_eq!(
        store.authorize_peer(&request),
        PeerGrantAuthorization::Authorized
    );
    let other_tab = PeerGrantRequest {
        tab_id: "tab-2".to_owned(),
        ..request.clone()
    };
    assert_eq!(
        store.authorize_peer(&other_tab),
        PeerGrantAuthorization::GrantUnavailable
    );

    clock.advance(Duration::from_millis(10_000));
    assert_eq!(
        store.authorize_peer(&request),
        PeerGrantAuthorization::Expired
    );
    assert!(admit_attachment_transfer_hello(&hello(&frame), &store, None).is_err());
    assert_eq!(store.current("g1"), None);
    let expired = changes
        .lock()
        .unwrap()
        .iter()
        .filter(|change| {
            matches!(
                change,
                GrantChange::Removed {
                    reason: GrantRemovalReason::Expired,
                    ..
                }
            )
        })
        .count();
    assert_eq!(expired, 1);
}

#[test]
fn replacement_names_the_previous_grant_and_revocation_takes_only_that_device() {
    let store = AttachmentGrantStore::new(EPOCH, TestClock::new().clock());
    let changes = recording(&store);
    store.install(&grant("g1", "dev-a")).unwrap();
    let replacement = DLocalAttachmentGrant {
        filename: "other.png".to_owned(),
        ..grant("g1", "dev-a")
    };
    store.install(&replacement).unwrap();
    store.install(&grant("g2", "dev-b")).unwrap();
    assert_eq!(store.revoke_device("dev-a"), 1);

    let changes = changes.lock().unwrap();
    let GrantChange::Installed {
        grant,
        previous: Some(previous),
    } = &changes[1]
    else {
        panic!("a replacement names what it replaced: {changes:?}");
    };
    assert_eq!(
        (grant.filename.as_str(), previous.filename.as_str()),
        ("other.png", "photo.png")
    );
    let GrantChange::Removed { grant, reason } = changes.last().unwrap() else {
        panic!("revocation is announced");
    };
    assert_eq!(
        (grant.grant_id.as_str(), *reason),
        ("g1", GrantRemovalReason::Revoked)
    );
    assert!(store.current("g2").is_some());
}
