//! The direct attachment receiver without a worker, coordinator or native
//! peer: dedicated-grant hello admission, per-chunk receipts, the committed
//! path, and grant expiry versus revocation. Ported from
//! `apps/worker/tests/attachments/attachment-direct-socket.test.ts`, one test
//! per v2 case with the v2 names kept.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "attachment_direct_support/mod.rs"]
mod attachment_direct_support;

use attachment_direct_support::{DirectFixture, acknowledgements, closed_reasons, frame_cases};
use roost_protocol::attachment_transfer::{GRANT_TTL_MS, MAX_ACTIVE_PER_WORKER};
use std::time::Duration;

#[tokio::test]
async fn rejects_a_direct_hello_whose_dedicated_secret_does_not_match() {
    let fixture = DirectFixture::new(1);
    let port = fixture.open();

    fixture.send_hello_with(&port, "wrong-secret");

    assert!(!port.is_live());
    assert_eq!(frame_cases(&port), vec!["closed"]);
    assert_eq!(closed_reasons(&port), vec!["grant_unavailable"]);
    assert!(!fixture.session_dir().exists());
}

#[tokio::test]
async fn writes_exact_bytes_then_acks_each_chunk_and_returns_the_committed_path() {
    let fixture = DirectFixture::new(5);
    let port = fixture.open();
    fixture.send_hello(&port);
    let first_digest = fixture.send_chunk(&port, 0, 0, &[1, 2], false).await;
    let final_digest = fixture.send_chunk(&port, 1, 2, &[3, 4, 5], true).await;

    let acks = acknowledgements(&port);
    assert_eq!(acks.len(), 2);
    assert_eq!(acks[0].upload_id, fixture.upload_id);
    assert_eq!((acks[0].seq, acks[0].bytes_received), (0, 2));
    assert_eq!(
        (acks[0].abs_path.as_str(), acks[0].error.as_str()),
        ("", "")
    );
    assert_eq!(acks[0].chunk_sha256, first_digest);
    let final_ack = &acks[1];
    assert_eq!((final_ack.seq, final_ack.bytes_received), (1, 5));
    assert_eq!(final_ack.error, "");
    assert_eq!(final_ack.chunk_sha256, final_digest);
    assert_eq!(
        std::fs::read(&final_ack.abs_path).unwrap(),
        vec![1, 2, 3, 4, 5]
    );
    let status = fixture.status();
    assert!(status.committed);
    assert_eq!(status.abs_path, final_ack.abs_path);
    assert_eq!(status.last_chunk_sha256, final_digest);
    assert_eq!(
        closed_reasons(&port).last().map(String::as_str),
        Some("complete")
    );
}

#[tokio::test]
async fn an_admitted_upload_completes_after_its_short_grant_expires_while_a_new_hello_is_refused() {
    let fixture = DirectFixture::new(3);
    let admitted = fixture.open();
    fixture.send_hello(&admitted);

    fixture.advance_grant_clock(Duration::from_millis(GRANT_TTL_MS + 1));
    assert!(fixture.grants.current(&fixture.grant_id).is_none());
    assert!(admitted.is_live());
    fixture.send_chunk(&admitted, 0, 0, &[7], false).await;
    fixture.send_chunk(&admitted, 1, 1, &[8, 9], true).await;
    assert_eq!(
        acknowledgements(&admitted).last().unwrap().bytes_received,
        3
    );

    let fresh = fixture.open();
    fixture.send_hello(&fresh);
    assert!(!fresh.is_live());
    assert_eq!(closed_reasons(&fresh), vec!["grant_unavailable"]);
}

#[tokio::test]
async fn explicit_device_revocation_closes_a_leased_port_after_grant_expiry() {
    let fixture = DirectFixture::new(1);
    let port = fixture.open();
    fixture.send_hello(&port);
    fixture.advance_grant_clock(Duration::from_millis(GRANT_TTL_MS + 1));
    assert!(fixture.grants.current(&fixture.grant_id).is_none());

    fixture
        .sockets
        .revoke_device(attachment_direct_support::DEVICE);

    assert!(!port.is_live());
    assert_eq!(
        closed_reasons(&port).last().map(String::as_str),
        Some("grant_unavailable")
    );
}

#[tokio::test]
async fn a_replayed_grant_cannot_admit_a_second_carrier_or_disturb_the_admitted_upload() {
    let fixture = DirectFixture::new(2);
    let admitted = fixture.open();
    fixture.send_hello(&admitted);
    fixture.send_chunk(&admitted, 0, 0, &[1], false).await;

    let replay = fixture.open();
    fixture.send_hello(&replay);
    assert!(!replay.is_live());
    assert_eq!(frame_cases(&replay), vec!["closed"]);

    fixture.send_chunk(&admitted, 1, 1, &[2], true).await;
    let final_ack = acknowledgements(&admitted).pop().unwrap();
    assert_eq!(std::fs::read(&final_ack.abs_path).unwrap(), vec![1, 2]);
}

#[tokio::test]
async fn an_admitted_upload_frees_its_pre_hello_slot_for_the_next_loopback_socket() {
    let fixture = DirectFixture::new(1);
    let idle: Vec<_> = (0..MAX_ACTIVE_PER_WORKER).map(|_| fixture.open()).collect();
    assert!(idle.iter().all(|port| port.is_live()));
    assert!(!fixture.open().is_live());

    fixture.send_hello(&idle[0]);
    assert!(idle[0].is_live());
    assert!(fixture.open().is_live());
}

/// No v2 counterpart asserts the loopback hello deadline; this pins v2's
/// `armLoopbackHelloDeadline` (3 s, then `invalid_hello`).
#[tokio::test(start_paused = true)]
async fn a_loopback_socket_that_never_says_hello_is_refused_at_the_deadline() {
    let fixture = DirectFixture::new(1);
    let port = fixture.open();
    tokio::time::sleep(Duration::from_millis(2_999)).await;
    assert!(port.is_live());
    tokio::time::sleep(Duration::from_millis(2)).await;
    assert!(!port.is_live());
    assert_eq!(closed_reasons(&port), vec!["invalid_hello"]);
}
