//! How a file is sliced, what a chunk has to prove before it settles, and what
//! a completed transfer has to verify before its path is recorded anywhere.
//!
//! The defect class these prevent is a client that believes a carrier: a chunk
//! boundary that silently shifts every later offset, a receipt believed for a
//! chunk the worker never wrote, and an upload whose card says `Done` for bytes
//! nobody acknowledged.
//!
//! The v2 names are the deliverable and are kept verbatim, so a reader holding
//! `apps/web/tests/client/attachments/attachmentTransfer.test.ts` can pair them
//! one for one.
//!
//! The mutation experiment for this file, in the slice report: in
//! `DirectUpload::settle`, delete the `ack.chunk_sha256 != chunk.chunk_sha256`
//! arm, and `a_completed_transfer_verifies_its_digest_before_the_path_is_recorded`
//! must fail.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::ClientCore;
use roost_client_core::client::attachments::transfer::ledger::{
    begin_upload_card, settle_upload_card,
};
use roost_client_core::client::attachments::transfer::receipt::{
    AttachmentTransferStatus, RECEIPT_SOURCES, ReceiptOutcome, ReceiptSource, settle_from_receipt,
};
use roost_client_core::client::attachments::transfer::{
    AttachmentTransferAck, DIRECT_CHUNK_BYTES, DirectUpload, InFlightChunk, SliceRequest,
};
use roost_client_core::store::transfers::TransferState;

/// One lowercase hex SHA-256, which is the only digest shape the pipeline
/// accepts. Two of them, so a test that mixes them up fails.
const FIRST_DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const SECOND_DIGEST: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

/// The path a worker commits to in these tests.
const WORKER_PATH: &str = "/worker/received.bin";

/// A file the host can slice, standing in for a browser `File`.
struct HostFile {
    bytes: Vec<u8>,
}

impl HostFile {
    fn of_size(size: usize) -> Self {
        Self {
            bytes: (0..size)
                .map(|index| ((index * 13 + 5) & 0xff) as u8)
                .collect(),
        }
    }

    /// The bytes at `request`, exactly as a host slicing a file would return
    /// them.
    fn slice(&self, request: &SliceRequest) -> Vec<u8> {
        let start = request.offset as usize;
        self.bytes[start..start + request.bytes].to_vec()
    }
}

/// The acknowledgement an accepting worker sends, and the digest rule it
/// applies: exactly the chunk's own digest, and a path only on the last chunk.
fn accepting_ack(chunk: &InFlightChunk) -> AttachmentTransferAck {
    AttachmentTransferAck {
        bytes_received: chunk.expected_bytes(),
        abs_path: if chunk.last {
            WORKER_PATH.to_owned()
        } else {
            String::new()
        },
        chunk_sha256: chunk.chunk_sha256.clone(),
    }
}

// ------------------------------------------------------------------ transfer

#[test]
fn sends_ordered_512_kib_byte_slices_and_advances_only_from_matching_acks() {
    let file = HostFile::of_size(DIRECT_CHUNK_BYTES as usize + 3);
    let mut upload = DirectUpload::new("upload-a", file.bytes.len() as u64);
    let mut framed = Vec::new();
    let mut progress = Vec::new();

    while let Some(request) = upload.next_slice() {
        let data = file.slice(&request);
        upload
            .begin_chunk(data.clone(), FIRST_DIGEST)
            .expect("the slice is the one that was asked for");
        let in_flight = upload.in_flight().expect("a chunk is in flight").clone();
        assert_eq!(in_flight.seq, request.seq);
        assert_eq!(in_flight.offset, request.offset);
        framed.push((in_flight.seq, in_flight.offset, in_flight.last, data));
        let settled = upload
            .settle(&accepting_ack(&in_flight))
            .expect("an accepting worker settles the chunk");
        progress.push(settled.bytes_received);
    }

    assert_eq!(
        framed
            .iter()
            .map(|(seq, offset, last, _)| (*seq, *offset, *last))
            .collect::<Vec<_>>(),
        vec![(0, 0, false), (1, DIRECT_CHUNK_BYTES, true)],
        "the boundary is the direct chunk size and the last flag rides the end of the file"
    );
    let lengths: Vec<usize> = framed.iter().map(|(_, _, _, data)| data.len()).collect();
    assert_eq!(
        lengths,
        vec![DIRECT_CHUNK_BYTES as usize, 3],
        "no slice is short but the last"
    );
    assert_eq!(
        progress,
        vec![DIRECT_CHUNK_BYTES, DIRECT_CHUNK_BYTES + 3],
        "progress is what the worker acknowledged, in order"
    );
    let reassembled: Vec<u8> = framed
        .iter()
        .flat_map(|(_, _, _, data)| data.clone())
        .collect();
    assert_eq!(
        reassembled, file.bytes,
        "the slices reassemble into the file"
    );
    assert_eq!(
        upload.outcome().map(|result| result.abs_path),
        Some(WORKER_PATH.to_owned())
    );
    assert_eq!(upload.close_reason(), "complete");
}

#[test]
fn preserves_a_zero_byte_file_as_one_final_direct_chunk() {
    let mut upload = DirectUpload::new("upload-a", 0);
    let request = upload
        .next_slice()
        .expect("a zero-byte file still has one chunk");
    assert_eq!(
        (request.seq, request.offset, request.bytes, request.last),
        (0, 0, 0, true),
        "a file with no chunks is a file that was never created"
    );
    upload
        .begin_chunk(Vec::new(), FIRST_DIGEST)
        .expect("an empty final slice is frameable");
    let in_flight = upload.in_flight().expect("a chunk is in flight").clone();
    let settled = upload
        .settle(&accepting_ack(&in_flight))
        .expect("the empty final chunk settles");
    assert_eq!(settled.bytes_received, 0);
    assert!(settled.completed);
    assert_eq!(
        upload.next_slice(),
        None,
        "no slice follows the final chunk"
    );
    assert_eq!(
        upload.outcome().map(|result| result.abs_path),
        Some(WORKER_PATH.to_owned())
    );
}

#[test]
fn settles_a_lost_final_ack_from_its_authenticated_direct_receipt() {
    // The worker wrote the final chunk and its acknowledgement was lost. The
    // receipt answers for it, so the chunk is NOT sent a second time.
    let mut upload = DirectUpload::new("upload-a", 2);
    upload
        .begin_chunk(vec![7, 8], FIRST_DIGEST)
        .expect("the only slice is frameable");
    let in_flight = upload.in_flight().expect("a chunk is in flight").clone();
    assert!(upload.sent_chunk(), "bytes are on the wire");

    let receipt = AttachmentTransferStatus {
        upload_id: "upload-a".to_owned(),
        next_seq: 1,
        bytes_received: 2,
        last_chunk_sha256: FIRST_DIGEST.to_owned(),
        committed: true,
        abs_path: "/worker/final-receipt.bin".to_owned(),
        error: String::new(),
    };
    assert_eq!(RECEIPT_SOURCES[0], ReceiptSource::Carrier);
    let ReceiptOutcome::Settled(ack) =
        settle_from_receipt(&in_flight, ReceiptSource::Carrier, Some(&receipt))
    else {
        panic!("the carrier's own receipt settles a chunk it already wrote");
    };
    let settled = upload.settle(&ack).expect("the recovered receipt settles");
    assert!(settled.completed);
    assert_eq!(
        upload.outcome().map(|result| result.abs_path),
        Some("/worker/final-receipt.bin".to_owned())
    );
}

#[test]
fn uses_coordinator_status_only_to_settle_a_lost_final_receipt_after_the_carrier_dies() {
    // The carrier is gone, so its own status cannot answer, and the
    // coordinator's durable receipt can.
    let mut upload = DirectUpload::new("upload-a", 1);
    upload
        .begin_chunk(vec![1], FIRST_DIGEST)
        .expect("the only slice is frameable");
    let in_flight = upload.in_flight().expect("a chunk is in flight").clone();

    assert_eq!(
        settle_from_receipt(&in_flight, ReceiptSource::Carrier, None),
        ReceiptOutcome::TryNextSource,
        "a carrier that cannot answer hands over to the next source"
    );
    let receipt = AttachmentTransferStatus {
        upload_id: "upload-a".to_owned(),
        next_seq: 1,
        bytes_received: 1,
        last_chunk_sha256: FIRST_DIGEST.to_owned(),
        committed: true,
        abs_path: "/worker/durable-final.bin".to_owned(),
        error: String::new(),
    };
    let ReceiptOutcome::Settled(ack) =
        settle_from_receipt(&in_flight, ReceiptSource::Coordinator, Some(&receipt))
    else {
        panic!("the coordinator's durable receipt settles it");
    };
    let settled = upload.settle(&ack).expect("the recovered receipt settles");
    assert!(settled.completed);
    assert_eq!(
        upload.outcome().map(|result| result.abs_path),
        Some("/worker/durable-final.bin".to_owned())
    );
}

#[test]
fn does_not_resume_a_nonfinal_direct_upload_through_coordinator_status() {
    // The first chunk's receipt says the worker has it but has NOT committed:
    // the file is still open. So the receipt does not settle that chunk, the
    // upload advances to the next slice, and the acknowledged chunk is never
    // sent again.
    let mut upload = DirectUpload::new("upload-a", DIRECT_CHUNK_BYTES + 1);
    let mut sent_sequences = Vec::new();

    while let Some(request) = upload.next_slice() {
        let data = vec![request.bytes as u8; request.bytes];
        upload
            .begin_chunk(data, FIRST_DIGEST)
            .expect("the slice is the one that was asked for");
        let in_flight = upload.in_flight().expect("a chunk is in flight").clone();
        sent_sequences.push(in_flight.seq);
        if sent_sequences.len() == 1 {
            let receipt = AttachmentTransferStatus {
                upload_id: "upload-a".to_owned(),
                next_seq: 1,
                bytes_received: DIRECT_CHUNK_BYTES,
                last_chunk_sha256: FIRST_DIGEST.to_owned(),
                committed: false,
                abs_path: String::new(),
                error: String::new(),
            };
            assert_eq!(
                settle_from_receipt(&in_flight, ReceiptSource::Carrier, Some(&receipt),),
                ReceiptOutcome::TryNextSource,
                "a receipt for an uncommitted chunk does not settle it"
            );
            // The direct route then dies outright, which is not ambiguous.
            continue;
        }
        let settled = upload
            .settle(&accepting_ack(&in_flight))
            .expect("the accepting worker settles the final chunk");
        assert!(settled.completed);
    }

    assert_eq!(
        sent_sequences,
        vec![0, 1],
        "the acknowledged chunk is not re-sent; the upload advances to the next slice"
    );
    assert_eq!(
        upload.outcome().map(|result| result.abs_path),
        Some(WORKER_PATH.to_owned())
    );
}

#[test]
fn a_completed_transfer_verifies_its_digest_before_the_path_is_recorded() {
    let mut core = ClientCore::in_memory("tab-attachments");
    begin_upload_card(core.store_mut(), "upload-a", "received.bin", 2, 0);
    let mut upload = DirectUpload::new("upload-a", 2);
    upload
        .begin_chunk(vec![1, 2], FIRST_DIGEST)
        .expect("the only slice is frameable");
    let in_flight = upload.in_flight().expect("a chunk is in flight").clone();

    // A receipt that echoes a DIFFERENT digest is refused, so the path is never
    // recorded and the card never settles.
    let forged = AttachmentTransferStatus {
        upload_id: "upload-a".to_owned(),
        next_seq: 1,
        bytes_received: 2,
        last_chunk_sha256: SECOND_DIGEST.to_owned(),
        committed: true,
        abs_path: WORKER_PATH.to_owned(),
        error: String::new(),
    };
    assert_eq!(
        settle_from_receipt(&in_flight, ReceiptSource::Carrier, Some(&forged)),
        ReceiptOutcome::TryNextSource
    );

    // And an acknowledgement carrying the wrong digest is refused outright.
    let wrong_digest = AttachmentTransferAck {
        bytes_received: 2,
        abs_path: WORKER_PATH.to_owned(),
        chunk_sha256: SECOND_DIGEST.to_owned(),
    };
    let refused = upload
        .settle(&wrong_digest)
        .expect_err("a forged digest is refused");
    assert!(
        refused.sent_chunk,
        "bytes did leave, so this is not a fallback"
    );
    assert_eq!(
        upload.outcome(),
        None,
        "no path is recorded for a refused chunk"
    );
    assert!(settle_upload_card(
        core.store_mut(),
        "upload-a",
        Err(&refused.reason),
        1
    ));
    assert_eq!(
        core.store()
            .transfers
            .transfer("upload-a")
            .map(|card| card.state),
        Some(TransferState::Failed),
        "a transfer whose digest did not verify is a failure, not a done card"
    );
}
