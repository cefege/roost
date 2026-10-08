#![cfg(unix)]
//! The durable half of an attachment operation: a crashed commit's final name
//! recovered without corrupting the manifest, a status that never reads the
//! bytes it describes, a failed flush that withholds the receipt, and a
//! journal that holds progress, never data. Ports those cases of v2
//! `apps/worker/tests/attachments/attachment-operation-owner.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod attachment_support;

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use attachment_support::{ChunkShape, Scratch, chunk, digest};
use roost_worker::attachments::file_store::{probe_attachment, record_attachment_hash};
use roost_worker::attachments::journal::{
    create_attachment_operation, create_attachment_operation_paths, persist_attachment_operation,
};
use roost_worker::attachments::operation_owner::AttachmentOperationOwner;
use roost_worker::attachments::receipts::AttachmentOperationError;
use roost_worker::attachments::store_paths::AttachmentBase;
use roost_worker::attachments::{Carrier, OperationDescriptor, system_clock};

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
async fn a_recovered_final_name_collision_is_refused_without_corrupting_the_manifest() {
    let scratch = Scratch::new("collision");
    let base = scratch.base();
    let expected = [1u8, 2, 3];
    let occupied = [4u8, 5, 6];
    let descriptor = OperationDescriptor {
        request_id: REQUEST.to_owned(),
        session_id: SESSION.to_owned(),
        filename: "collision.bin".to_owned(),
        short_path: false,
        total_bytes: Some(3),
    };
    let created = create_attachment_operation(&base, &descriptor, Carrier::Direct, "socket-a")
        .unwrap()
        .unwrap();
    let (paths, mut journal) = (created.paths, created.journal);
    let destination = paths.media_dir.join("collision.bin");
    std::fs::write(&paths.temp_path, expected).unwrap();
    std::fs::write(&destination, occupied).unwrap();
    record_attachment_hash(&paths.media_dir, &digest(&occupied), "collision.bin");
    journal.next_seq = 1;
    journal.bytes_written = 3;
    journal.last_chunk_final = true;
    journal.last_chunk_sha256 = digest(&expected);
    journal.final_name = "collision.bin".to_owned();
    journal.content_sha256 = digest(&expected);
    persist_attachment_operation(&paths, &journal, true).unwrap();

    let status = owner(&base).status(SESSION, REQUEST);
    assert_eq!(
        (status.committed, status.error),
        (false, Some(AttachmentOperationError::WriteFailed))
    );
    assert_eq!(std::fs::read(&destination).unwrap(), occupied);
    assert!(probe_attachment(&base, SESSION, &digest(&occupied), false).hit);
    assert!(!probe_attachment(&base, SESSION, &digest(&expected), false).hit);
}

/// v2 spies on `readSync`; here the temp is made unreadable, so a status that
/// reopened or rehashed it would fail instead of answering from the journal.
#[tokio::test]
async fn the_status_of_a_detached_partial_upload_never_reads_its_bytes() {
    let scratch = Scratch::new("status");
    let base = scratch.base();
    let first = owner(&base);
    first
        .accept(chunk(SESSION, REQUEST, &[0; 4096], total(8192)))
        .outcome()
        .await
        .unwrap();
    first.detach_direct_carrier("socket-a");
    let temp =
        create_attachment_operation_paths(&base, SESSION, REQUEST, &base.session_dir(SESSION))
            .unwrap()
            .temp_path;
    std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o000)).unwrap();

    let status = owner(&base).status(SESSION, REQUEST);
    assert_eq!(
        (status.next_seq, status.bytes_received, status.error),
        (1, 4096, None)
    );
}

#[tokio::test]
async fn a_failed_directory_flush_withholds_the_final_receipt() {
    let scratch = Scratch::new("dirflush");
    let base = scratch.base();
    let owner = owner(&base);
    let first = owner
        .accept(chunk(SESSION, REQUEST, &[6], total(2)))
        .outcome()
        .await
        .unwrap();
    assert!(!first.committed);
    let operation_dir =
        create_attachment_operation_paths(&base, SESSION, REQUEST, &base.session_dir(SESSION))
            .unwrap()
            .operation_dir;
    // Writable and searchable but not readable: every step of the commit works
    // except opening the directory to flush it.
    std::fs::set_permissions(&operation_dir, std::fs::Permissions::from_mode(0o300)).unwrap();
    if std::fs::File::open(&operation_dir).is_ok() {
        eprintln!("skipped: this user can open an unreadable directory");
        return;
    }
    let last = ChunkShape {
        seq: 1,
        offset: 1,
        last: true,
        total_bytes: Some(2),
        ..ChunkShape::default()
    };
    let refused = owner
        .accept(chunk(SESSION, REQUEST, &[7], last))
        .outcome()
        .await;
    std::fs::set_permissions(&operation_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(refused, Err(AttachmentOperationError::WriteFailed));
    assert!(!owner.status(SESSION, REQUEST).committed);
}

#[tokio::test]
async fn the_journal_records_metadata_and_progress_never_chunk_bytes() {
    let scratch = Scratch::new("journal");
    let base = scratch.base();
    let relay = ChunkShape {
        total_bytes: Some(128 * 1024),
        ..ChunkShape::relay()
    };
    owner(&base)
        .accept(chunk(SESSION, REQUEST, &vec![7; 64 * 1024], relay))
        .outcome()
        .await
        .unwrap();
    let journal_path =
        create_attachment_operation_paths(&base, SESSION, REQUEST, &base.session_dir(SESSION))
            .unwrap()
            .journal_path;
    let text = std::fs::read_to_string(journal_path).unwrap();
    assert!(text.len() < 2_048);
    let journal: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(journal.get("data").is_none());
    assert_eq!(
        journal["requestId"], REQUEST,
        "the journal keeps v2's on-disk field names"
    );
}
