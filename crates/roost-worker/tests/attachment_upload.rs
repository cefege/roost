//! The coordinator relay upload: multi-chunk byte fidelity, v2's final naming
//! and duplicate suffixes, an empty file, the short-path link, the refusals
//! that leave no file behind, and the content-dedup manifest. Ports v2
//! `apps/worker/tests/attachments/attachment-upload.test.ts` (its sanitizer
//! cases are unit tests of `attachments::naming`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod attachment_support;

use attachment_support::{Scratch, digest};
use roost_proto::DAttachmentChunk;
use roost_worker::attachments::file_store::{probe_attachment, record_attachment_hash};
use roost_worker::attachments::receipts::AttachmentOperationError;
use roost_worker::attachments::system_clock;
use roost_worker::attachments::upload::{AttachmentOperations, RelayChunkOutcome};

const SESSION: &str = "test-upload-session";

struct Relay {
    scratch: Scratch,
    operations: AttachmentOperations,
}

fn relay(label: &str) -> Relay {
    let scratch = Scratch::new(label);
    let operations = AttachmentOperations::new(scratch.base(), system_clock());
    Relay {
        scratch,
        operations,
    }
}

fn relay_chunk(
    request_id: &str,
    session_id: &str,
    filename: &str,
    data: &[u8],
    seq: u32,
    last: bool,
) -> DAttachmentChunk {
    DAttachmentChunk {
        request_id: request_id.to_owned(),
        session_id: session_id.to_owned(),
        filename: filename.to_owned(),
        short_path: false,
        data: data.to_vec(),
        last,
        seq,
        ..Default::default()
    }
}

impl Relay {
    /// Every chunk is offered before any answer is awaited, as the link does.
    async fn feed(
        &self,
        request_id: &str,
        filename: &str,
        slices: &[&[u8]],
        short_path: bool,
    ) -> RelayChunkOutcome {
        let mut answers = Vec::new();
        for (index, data) in slices.iter().enumerate() {
            let last = index + 1 == slices.len();
            let mut chunk = relay_chunk(
                request_id,
                SESSION,
                filename,
                data,
                u32::try_from(index).unwrap(),
                last,
            );
            chunk.short_path = short_path;
            answers.push(self.operations.accept_relay_chunk(chunk));
        }
        let mut outcome = RelayChunkOutcome::Progress;
        for answer in answers {
            outcome = answer.await;
        }
        outcome
    }

    async fn saved(&self, request_id: &str, filename: &str, slices: &[&[u8]]) -> String {
        match self.feed(request_id, filename, slices, false).await {
            RelayChunkOutcome::Saved { abs_path } => abs_path,
            other => panic!("the upload was not saved: {other:?}"),
        }
    }

    fn dir(&self) -> std::path::PathBuf {
        self.scratch.base().session_dir(SESSION)
    }
}

#[tokio::test]
async fn controls_are_stripped_without_changing_the_leaf_or_its_compound_extension() {
    let relay = relay("controls");
    let inert = "report ' \"$() `tick` 雪";
    let raw = format!("ignored/\r\n\u{1b}{inert}\u{85}\u{7f}.tar\u{9f}.gz\u{1}");
    let abs_path = relay.saved("r-controls", &raw, &[b"safe"]).await;
    assert!(
        abs_path.ends_with(&format!("/{inert}.tar.gz")),
        "{abs_path}"
    );
    assert!(
        !abs_path
            .chars()
            .any(|character| character <= '\u{1f}' || ('\u{7f}'..='\u{9f}').contains(&character))
    );
    assert_eq!(std::fs::read_to_string(&abs_path).unwrap(), "safe");
}

#[tokio::test]
async fn a_multi_chunk_upload_assembles_byte_exact_in_order() {
    let relay = relay("multi");
    let total: Vec<u8> = (0..2_500_000u32)
        .map(|index| (index & 0xff) as u8)
        .collect();
    let slices: Vec<&[u8]> = total.chunks(1024 * 1024).collect();
    let abs_path = relay.saved("r-multi", "blob.bin", &slices).await;
    assert_eq!(std::fs::read(&abs_path).unwrap(), total);
}

#[tokio::test]
async fn the_original_name_is_kept_and_duplicates_get_a_numbered_suffix() {
    let relay = relay("dup");
    let first = relay.saved("r-dup-1", "dup.txt", &[b"first"]).await;
    let second = relay.saved("r-dup-2", "dup.txt", &[b"second"]).await;
    let third = relay.saved("r-dup-3", "dup.txt", &[b"third"]).await;
    assert!(first.ends_with("/dup.txt"));
    assert!(second.ends_with("/dup (2).txt"));
    assert!(third.ends_with("/dup (3).txt"));
    assert_eq!(std::fs::read_to_string(&first).unwrap(), "first");
    assert_eq!(std::fs::read_to_string(&second).unwrap(), "second");
}

#[tokio::test]
async fn a_single_empty_last_chunk_creates_an_empty_file() {
    let relay = relay("empty");
    let abs_path = relay.saved("r-empty", "empty.txt", &[b""]).await;
    assert_eq!(std::fs::metadata(&abs_path).unwrap().len(), 0);
}

#[tokio::test]
async fn a_short_path_upload_is_answered_with_a_shortcut_link_to_the_file() {
    let relay = relay("short");
    let RelayChunkOutcome::Saved { abs_path } =
        relay.feed("r-short", "note.txt", &[b"hello"], true).await
    else {
        panic!("the upload was not saved");
    };
    assert!(abs_path.contains("/.shortcuts/p1"), "{abs_path}");
    assert_eq!(std::fs::read_to_string(&abs_path).unwrap(), "hello");
}

#[tokio::test]
async fn a_session_id_that_climbs_out_of_the_base_is_refused_and_writes_nothing() {
    let relay = relay("traversal");
    let chunk = relay_chunk("r-escape", "../../etc", "x", &[1], 0, true);
    let outcome = relay.operations.accept_relay_chunk(chunk).await;
    assert_eq!(
        outcome,
        RelayChunkOutcome::Failed(AttachmentOperationError::UploadNotFound)
    );
    assert_eq!(
        AttachmentOperationError::UploadNotFound.message(),
        "upload is unavailable"
    );
    assert!(!relay.scratch.root.join("etc").exists());
}

/// Silent-truncation guard: a continuation with no operation behind it (its
/// predecessor failed, or was swept) is refused, never saved as a whole file.
#[tokio::test]
async fn a_continuation_chunk_with_no_operation_behind_it_is_refused() {
    let relay = relay("ghost");
    let chunk = relay_chunk("r-ghost", SESSION, "ghost.bin", &[9, 9, 9], 1, true);
    let outcome = relay.operations.accept_relay_chunk(chunk).await;
    assert_eq!(
        outcome,
        RelayChunkOutcome::Failed(AttachmentOperationError::ChunkOutOfOrder)
    );
    assert!(!relay.dir().join("ghost.bin").exists());
}

#[tokio::test]
async fn an_out_of_order_chunk_aborts_the_upload_and_removes_its_temp() {
    let relay = relay("ooo");
    let opened = relay.operations.accept_relay_chunk(relay_chunk(
        "r-ooo",
        SESSION,
        "ooo.bin",
        &[1, 2, 3],
        0,
        false,
    ));
    assert_eq!(opened.await, RelayChunkOutcome::Progress);
    let skipped = relay.operations.accept_relay_chunk(relay_chunk(
        "r-ooo",
        SESSION,
        "ooo.bin",
        &[4, 5, 6],
        2,
        true,
    ));
    assert_eq!(
        skipped.await,
        RelayChunkOutcome::Failed(AttachmentOperationError::ChunkOutOfOrder)
    );
    assert!(!relay.dir().join("ooo.bin").exists());
    assert!(!relay.dir().join(".operations").join("r-ooo.part").exists());
}

#[tokio::test]
async fn an_upload_records_its_digest_and_the_probe_hits_its_path() {
    let relay = relay("dedup");
    let bytes: Vec<u8> = (0..300_000u32)
        .map(|index| (index.wrapping_mul(7) & 0xff) as u8)
        .collect();
    let abs_path = relay.saved("r-dedup", "dedup.bin", &[&bytes]).await;
    let probe = probe_attachment(&relay.scratch.base(), SESSION, &digest(&bytes), false);
    assert_eq!((probe.hit, probe.abs_path), (true, abs_path));
    let never = probe_attachment(&relay.scratch.base(), SESSION, &"0".repeat(64), false);
    assert_eq!((never.hit, never.abs_path.as_str()), (false, ""));
}

#[tokio::test]
async fn the_probe_misses_once_the_recorded_file_is_gone() {
    let relay = relay("gone");
    let abs_path = relay.saved("r-gone", "gone.txt", &[b"ephemeral"]).await;
    let sha = digest(b"ephemeral");
    assert!(probe_attachment(&relay.scratch.base(), SESSION, &sha, false).hit);
    std::fs::remove_file(&abs_path).unwrap();
    assert!(!probe_attachment(&relay.scratch.base(), SESSION, &sha, false).hit);
}

#[tokio::test]
async fn recording_a_name_with_new_content_prunes_its_stale_digest() {
    let relay = relay("prune");
    let dir = relay.dir();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("over.txt"), "v2").unwrap();
    let (old, new) = ("a".repeat(64), "b".repeat(64));
    record_attachment_hash(&dir, &old, "over.txt");
    record_attachment_hash(&dir, &new, "over.txt");
    assert!(!probe_attachment(&relay.scratch.base(), SESSION, &old, false).hit);
    let hit = probe_attachment(&relay.scratch.base(), SESSION, &new, false);
    assert_eq!(hit.abs_path, dir.join("over.txt").to_string_lossy());
}
