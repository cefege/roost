//! The attachment operation's lifecycle: the carrier fence, the committed
//! receipt that answers a lost direct acknowledgement after a restart, the idle
//! sweep, and a resumed upload's bytes. Ports v2
//! `apps/worker/tests/attachments/attachment-operation-owner.test.ts` (its
//! durability cases are `attachment_operation_recovery`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod attachment_support;

use std::sync::Arc;

use attachment_support::{ChunkShape, Scratch, TestClock, chunk, digest};
use roost_worker::attachments::file_store::probe_attachment;
use roost_worker::attachments::journal::create_attachment_operation_paths;
use roost_worker::attachments::operation_owner::{
    ATTACHMENT_OPERATION_IDLE, AttachmentOperationOwner,
};
use roost_worker::attachments::receipts::AttachmentOperationError;
use roost_worker::attachments::store_paths::AttachmentBase;
use roost_worker::attachments::system_clock;

const SESSION: &str = "test-session";
const REQUEST: &str = "11111111-2222-4333-8444-555555555555";

fn owner(base: &AttachmentBase) -> Arc<AttachmentOperationOwner> {
    AttachmentOperationOwner::new(base.clone(), system_clock())
}

fn total(bytes: u64) -> ChunkShape {
    ChunkShape {
        total_bytes: Some(bytes),
        ..ChunkShape::default()
    }
}

#[tokio::test]
async fn a_coordinator_continuation_of_a_direct_upload_is_refused_and_only_status_survives() {
    let scratch = Scratch::new("carrier");
    let base = scratch.base();
    let first = owner(&base);
    let receipt = first
        .accept(chunk(SESSION, REQUEST, &[1], total(2)))
        .outcome()
        .await
        .unwrap();
    assert_eq!((receipt.next_seq, receipt.bytes_received), (1, 1));
    first.detach_direct_carrier("socket-a");

    let restarted = owner(&base);
    let status = restarted.status(SESSION, REQUEST);
    assert_eq!(
        (status.next_seq, status.bytes_received, status.committed),
        (1, 1, false)
    );
    assert_eq!(status.last_chunk_sha256, digest(&[1]));
    let relay = ChunkShape {
        seq: 1,
        offset: 1,
        last: true,
        total_bytes: Some(2),
        ..ChunkShape::relay()
    };
    let fallback = restarted
        .accept(chunk(SESSION, REQUEST, &[2], relay))
        .outcome()
        .await;
    assert_eq!(fallback, Err(AttachmentOperationError::UploadMismatch));
    let status = restarted.status(SESSION, REQUEST);
    assert_eq!(
        (
            status.next_seq,
            status.bytes_received,
            status.committed,
            status.error
        ),
        (1, 1, false, None)
    );
    assert!(!base.session_dir(SESSION).join("carrier.bin").exists());
}

#[tokio::test]
async fn an_exact_direct_duplicate_after_restart_gets_the_durable_final_receipt() {
    let scratch = Scratch::new("receipt");
    let base = scratch.base();
    let data = [9, 8, 7];
    let whole = ChunkShape {
        last: true,
        total_bytes: Some(3),
        ..ChunkShape::default()
    };
    let initial = owner(&base)
        .accept(chunk(SESSION, REQUEST, &data, whole.clone()))
        .outcome()
        .await;
    let receipt = initial.clone().unwrap();
    assert!(receipt.committed);

    let restarted = owner(&base);
    let duplicate = restarted
        .accept(chunk(SESSION, REQUEST, &data, whole))
        .outcome()
        .await;
    assert_eq!(duplicate, initial);
    let status = restarted.status(SESSION, REQUEST);
    assert!(status.committed);
    assert_eq!(
        (status.abs_path, status.last_chunk_sha256),
        (receipt.abs_path, digest(&data))
    );
}

/// The behaviour half of v2's "never blocks the event loop on fsync" case: a
/// direct chunk's progress is journaled for a fresh owner to read, and a relay
/// commit lands every byte in order.
#[tokio::test]
async fn direct_progress_is_journaled_and_a_relay_commit_lands_every_byte() {
    let scratch = Scratch::new("flush");
    let base = scratch.base();
    let direct = owner(&base);
    for (seq, byte) in [(0u32, 1u8), (1, 2)] {
        let shape = ChunkShape {
            seq,
            offset: u64::from(seq),
            total_bytes: Some(3),
            ..ChunkShape::default()
        };
        let receipt = direct
            .accept(chunk(SESSION, "direct", &[byte], shape))
            .outcome()
            .await
            .unwrap();
        assert_eq!(receipt.next_seq, seq + 1);
    }
    let status = owner(&base).status(SESSION, "direct");
    assert_eq!((status.next_seq, status.bytes_received), (2, 2));
    direct.detach_direct_carrier("socket-a");

    let relay = owner(&base);
    for (seq, byte) in [(0u32, 3u8), (1, 4)] {
        let shape = ChunkShape {
            seq,
            offset: u64::from(seq),
            total_bytes: Some(3),
            ..ChunkShape::relay()
        };
        relay
            .accept(chunk("relay-session", "relay", &[byte], shape))
            .outcome()
            .await
            .unwrap();
    }
    let last = ChunkShape {
        seq: 2,
        offset: 2,
        last: true,
        total_bytes: Some(3),
        ..ChunkShape::relay()
    };
    let receipt = relay
        .accept(chunk("relay-session", "relay", &[5], last))
        .outcome()
        .await
        .unwrap();
    assert!(receipt.committed);
    assert_eq!(std::fs::read(&receipt.abs_path).unwrap(), [3, 4, 5]);
}

#[tokio::test]
async fn the_idle_sweep_fails_a_silent_relay_and_parks_a_silent_direct_upload() {
    let scratch = Scratch::new("idle");
    let base = scratch.base();
    let clock = TestClock::new();
    let owner = AttachmentOperationOwner::new(base.clone(), clock.clock());
    let relay = ChunkShape {
        total_bytes: Some(2),
        ..ChunkShape::relay()
    };
    owner
        .accept(chunk(SESSION, "relay", &[1], relay.clone()))
        .outcome()
        .await
        .unwrap();
    owner
        .accept(chunk(SESSION, "direct", &[1], total(2)))
        .outcome()
        .await
        .unwrap();
    clock.advance(ATTACHMENT_OPERATION_IDLE);
    owner
        .accept(chunk(SESSION, "fresh", &[1], relay.clone()))
        .outcome()
        .await
        .unwrap();
    clock.advance(std::time::Duration::from_millis(1));
    owner.sweep_idle();

    let relay_paths = create_attachment_operation_paths(&base, SESSION, "relay").unwrap();
    assert!(!relay_paths.temp_path.exists());
    let resumed = ChunkShape {
        seq: 1,
        offset: 1,
        last: true,
        ..relay
    };
    let refused = owner
        .accept(chunk(SESSION, "relay", &[2], resumed.clone()))
        .outcome()
        .await;
    assert_eq!(refused, Err(AttachmentOperationError::UploadNotFound));
    let status = owner.status(SESSION, "direct");
    assert_eq!(
        (status.next_seq, status.bytes_received, status.error),
        (1, 1, None)
    );
    let direct_last = ChunkShape {
        seq: 1,
        offset: 1,
        last: true,
        total_bytes: Some(2),
        ..ChunkShape::default()
    };
    let committed = owner
        .accept(chunk(SESSION, "direct", &[2], direct_last))
        .outcome()
        .await
        .unwrap();
    assert!(committed.committed);
    let fresh = owner
        .accept(chunk(SESSION, "fresh", &[2], resumed))
        .outcome()
        .await
        .unwrap();
    assert!(fresh.committed);
}

/// v2 defect, fixed deliberately: v2 reopened a parked temp `r+` and wrote at
/// offset 0, so a resumed upload overwrote its own head.
#[tokio::test]
async fn a_resumed_direct_upload_appends_after_the_bytes_it_already_holds() {
    let scratch = Scratch::new("resume");
    let base = scratch.base();
    let first = owner(&base);
    first
        .accept(chunk(SESSION, REQUEST, &[1, 2, 3], total(6)))
        .outcome()
        .await
        .unwrap();
    first.detach_direct_carrier("socket-a");

    let last = ChunkShape {
        seq: 1,
        offset: 3,
        last: true,
        total_bytes: Some(6),
        ..ChunkShape::default()
    };
    let receipt = owner(&base)
        .accept(chunk(SESSION, REQUEST, &[4, 5, 6], last))
        .outcome()
        .await
        .unwrap();
    assert!(receipt.committed);
    assert_eq!(
        std::fs::read(&receipt.abs_path).unwrap(),
        [1, 2, 3, 4, 5, 6]
    );
    let probe = probe_attachment(&base, SESSION, &digest(&[1, 2, 3, 4, 5, 6]), false);
    assert_eq!((probe.hit, probe.abs_path), (true, receipt.abs_path));
}
