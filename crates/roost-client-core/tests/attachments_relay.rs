//! The carrier that is always left: the coordinator relay.
//!
//! Every direct refusal ends here, so this route has no admission of its own and
//! no digest to check — a worker relay chunk is a Connect call, not an
//! authenticated channel. What it does still owe the user is v2's exact chunk
//! shape, its exact ordering, and its exact progress accounting, because a
//! change to any of those is a change every existing upload sees.
//!
//! The v2 names are kept verbatim, from
//! `apps/web/tests/attachmentsDirectFallback.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "attachment_support/mod.rs"]
mod attachment_support;
use attachment_support::{FakeEnvironment, block_on};
use roost_client_core::client::attachments::direct::relay::{RELAY_CHUNK_BYTES, RelayUpload};
use roost_client_core::client::attachments::direct::{
    AttachmentDirectUploadRequest, DirectAttempt, DirectUnavailableReason, upload_attachment_direct,
};

// ------------------------------------------------------- the relay fallback

#[test]
fn retains_existing_relay_chunk_fields_and_ordered_progress_when_direct_is_unavailable() {
    let total = RELAY_CHUNK_BYTES as usize + 1;
    let bytes: Vec<u8> = (0..total)
        .map(|index| ((index * 7 + 3) & 0xff) as u8)
        .collect();
    let mut environment = FakeEnvironment {
        door: None,
        peer_available: false,
        ..FakeEnvironment::new()
    };
    let upload_request = AttachmentDirectUploadRequest {
        session_id: "session-a".to_owned(),
        worker_fp: Some("worker-a".to_owned()),
        upload_id: "upload-a".to_owned(),
        file_name: "relay.bin".to_owned(),
        file_bytes: total as u64,
        short_path: false,
    };
    assert_eq!(
        block_on(upload_attachment_direct(&upload_request, &mut environment)),
        DirectAttempt::Unavailable(DirectUnavailableReason::NoLocalCarrier)
    );

    let mut relay = RelayUpload::new(&upload_request);
    let mut sent = Vec::new();
    let mut progress = Vec::new();
    while let Some(slice) = relay.next_slice() {
        let data = bytes[slice.offset as usize..slice.offset as usize + slice.bytes].to_vec();
        let chunk = relay.frame(data).expect("a slice was asked for");
        sent.push((
            chunk.session_id.clone(),
            chunk.filename.clone(),
            chunk.short_path,
            chunk.seq,
            chunk.last,
            chunk.data.len(),
        ));
        // The worker commits only on the final chunk, and the relay reports its
        // own slice boundary as progress.
        let abs_path = if chunk.last { "/worker/relay.bin" } else { "" };
        progress.push(relay.settle(abs_path));
    }

    assert_eq!(
        sent,
        vec![
            (
                "session-a".to_owned(),
                "relay.bin".to_owned(),
                false,
                0,
                false,
                RELAY_CHUNK_BYTES as usize
            ),
            (
                "session-a".to_owned(),
                "relay.bin".to_owned(),
                false,
                1,
                true,
                1
            ),
        ],
        "the relay's chunk fields and their order are unchanged"
    );
    assert_eq!(progress, vec![RELAY_CHUNK_BYTES, total as u64]);
    assert_eq!(
        relay.outcome().map(|result| result.abs_path),
        Some("/worker/relay.bin".to_owned())
    );
}

#[test]
fn keeps_a_zero_byte_file_as_one_final_coordinator_relay_call() {
    let upload_request = AttachmentDirectUploadRequest {
        worker_fp: Some("worker-a".to_owned()),
        session_id: "session-empty".to_owned(),
        upload_id: "upload-empty".to_owned(),
        file_name: "empty.bin".to_owned(),
        file_bytes: 0,
        short_path: false,
    };
    let mut relay = RelayUpload::new(&upload_request);

    let slice = relay
        .next_slice()
        .expect("a zero-byte file still has one chunk");
    assert_eq!(
        (slice.seq, slice.offset, slice.bytes, slice.last),
        (0, 0, 0, true)
    );
    let chunk = relay
        .frame(Vec::new())
        .expect("the empty final slice is frameable");
    assert_eq!(chunk.upload_id, "upload-empty");
    assert_eq!(chunk.seq, 0);
    assert!(chunk.last);
    assert!(chunk.data.is_empty());
    assert_eq!(
        relay.settle("/worker/empty.bin"),
        0,
        "a zero-byte file reports no progress"
    );
    assert_eq!(relay.next_slice(), None);
    assert_eq!(
        relay.outcome().map(|result| result.abs_path),
        Some("/worker/empty.bin".to_owned())
    );
}
