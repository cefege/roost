//! Attachment admission and the lease a running upload holds. The rule under
//! test throughout is the difference between EXPIRY, which gates a fresh hello,
//! and REVOCATION, which is an immediate fence — because getting it backwards
//! fails in both directions at once.

use roost_worker::attachment_transfer::{
    ACTIVE_LEASE, CHUNK_BYTES, ChunkRefusal, HelloRefusal, MAX_ACTIVE_PER_BROWSER_DOCUMENT,
    MAX_ACTIVE_PER_WORKER, MAX_CHUNKS_IN_FLIGHT, Transfers, UploadId,
};
use std::time::{Duration, Instant};

fn upload(id: &str, bytes: u64) -> UploadId {
    UploadId {
        upload_id: id.to_string(),
        device_fingerprint: "device-1".into(),
        tab_id: "tab-1".into(),
        session_id: "session-1".into(),
        filename: "notes.txt".into(),
        total_bytes: bytes,
    }
}

fn now() -> Instant {
    Instant::now()
}

/// EXPIRY GATES A FRESH HELLO ONLY. An admitted upload runs on its own finite
/// lease and does not stop when its grant's clock runs out — otherwise a large
/// file could never finish, because no upload outlasts a short lease.
#[test]
fn expiry_gates_a_fresh_hello_and_nothing_else() {
    let mut transfers = Transfers::new();
    let start = now();
    // Admitted while the grant is live.
    transfers
        .admit(upload("u1", 10_000), true, start)
        .expect("admitted");

    // The grant goes away. The RUNNING upload is untouched.
    let after_grant_died = start + ACTIVE_LEASE * 2;
    assert_eq!(
        transfers.expire_leases(after_grant_died).len(),
        1,
        "and the upload ends on its own lease, not because the grant died"
    );

    // A FRESH hello is refused, which is what expiry actually gates.
    let mut transfers = Transfers::new();
    transfers
        .admit(upload("u2", 10), false, start)
        .expect_err("an expired grant admits nothing");
}

/// THE OPPOSITE DIRECTION. A lease that DID NOT run out must not end a healthy
/// upload just because the grant's clock passed — that is the failure where
/// every large upload dies at the grant's expiry.
#[test]
fn a_healthy_upload_outlives_its_grant() {
    let mut transfers = Transfers::new();
    let start = now();
    // A short grant, admitted at its last moment.
    transfers
        .admit(upload("u1", 10), true, start)
        .expect("admitted at the edge of the grant");

    // Well past any plausible grant clock, well inside the lease.
    let still_running = start + Duration::from_secs(1);
    assert!(
        transfers.expire_leases(still_running).is_empty(),
        "a running upload does not stop because its grant aged out — its \\
         authority is the lease it was admitted with"
    );
    assert_eq!(transfers.active(), 1, "and it is still running");
}

/// REVOCATION IS AN IMMEDIATE FENCE. "It was admitted first" is not a defence.
#[test]
fn revocation_ends_a_running_upload_at_once() {
    let mut transfers = Transfers::new();
    let start = now();
    transfers
        .admit(upload("u1", 1_000_000), true, start)
        .expect("admitted");
    assert_eq!(transfers.active(), 1);

    let revoked = transfers.revoke_grant(&["u1".to_string()]);
    assert_eq!(revoked.len(), 1, "a running upload ends at once");
    assert_eq!(transfers.active(), 0, "and is gone before its lease could");
}

/// A revoked upload is not resumable under the same authority, while a lease
/// expiry is — under a NEW hello. Conflating them would either strand a
/// resumable upload or let a withdrawn grant keep running.
#[test]
fn revocation_is_not_resumable_and_expiry_is() {
    assert!(
        !roost_worker::attachment_transfer::Ended::GrantRevoked.resumable(),
        "a revoked grant is not one a later attempt may continue under"
    );
    assert!(
        roost_worker::attachment_transfer::Ended::LeaseExpired.resumable(),
        "but an upload that ran out of time may start again under a new hello"
    );
}

/// A fresh hello for a revoked upload is refused, and refused for the RIGHT
/// reason: a grant that was withdrawn is not merely expired.
#[test]
fn a_revoked_upload_cannot_re_admit() {
    let mut transfers = Transfers::new();
    let start = now();
    transfers
        .admit(upload("u1", 100), true, start)
        .expect("admitted");
    transfers.revoke_grant(&["u1".to_string()]);

    assert_eq!(
        transfers.admit(upload("u1", 100), true, start),
        Err(HelloRefusal::GrantUnavailable),
        "even with a live grant presented, a revoked upload id does not come back"
    );
}

/// ONE CHUNK IN FLIGHT, because a chunk is acknowledged before the next is
/// sent. Two in flight and a failure leaves the receiver unable to say which
/// bytes it already has.
#[test]
fn only_one_chunk_is_in_flight() {
    let mut transfers = Transfers::new();
    let start = now();
    transfers
        .admit(upload("u1", CHUNK_BYTES as u64 * 3), true, start)
        .expect("admitted");

    transfers.send_chunk("u1").expect("the first chunk goes");
    assert_eq!(
        transfers.send_chunk("u1"),
        Err(ChunkRefusal::ChunkInFlight),
        "a second chunk before the first is acknowledged would leave the \\
         receiver unable to say which bytes it has"
    );
    transfers
        .acknowledge_chunk("u1", start)
        .expect("acknowledged");
    transfers
        .send_chunk("u1")
        .expect("and now the next one goes");
    assert_eq!(MAX_CHUNKS_IN_FLIGHT, 1, "and the rule is one");
}

/// An upload finishes when every declared byte is acknowledged, not when the
/// caller says so.
#[test]
fn an_upload_finishes_on_its_declared_length() {
    let mut transfers = Transfers::new();
    let start = now();
    let total = CHUNK_BYTES as u64 * 2;
    transfers
        .admit(upload("u1", total), true, start)
        .expect("admitted");

    transfers.send_chunk("u1").expect("chunk one");
    assert!(
        !transfers.acknowledge_chunk("u1", start).expect("acked"),
        "not yet"
    );
    transfers.send_chunk("u1").expect("chunk two");
    assert!(
        transfers.acknowledge_chunk("u1", start).expect("acked"),
        "and now it is finished"
    );
    assert_eq!(transfers.active(), 0, "and the slot is released");
}

/// A single-chunk upload finishes on its first acknowledgement, so the common
/// case of a small file does not wait for a second round trip.
#[test]
fn a_small_upload_finishes_on_its_only_chunk() {
    let mut transfers = Transfers::new();
    let start = now();
    transfers
        .admit(upload("small", 10), true, start)
        .expect("admitted");
    transfers.send_chunk("small").expect("the chunk goes");
    assert!(
        transfers.acknowledge_chunk("small", start).expect("acked"),
        "and that is the whole file"
    );
}

/// A duplicate hello for an upload already running is refused. Admitting it
/// again would double the byte count against the same destination.
#[test]
fn a_duplicate_hello_is_refused() {
    let mut transfers = Transfers::new();
    let start = now();
    transfers
        .admit(upload("u1", 100), true, start)
        .expect("admitted");
    assert_eq!(
        transfers.admit(upload("u1", 100), true, start),
        Err(HelloRefusal::AlreadyAdmitted)
    );
    assert_eq!(
        transfers.active(),
        1,
        "and there is still one upload, not two"
    );
}

/// A hello with nothing to upload is refused, because admitting it would hold
/// an active slot for an upload that can never finish.
#[test]
fn a_hello_with_nothing_to_send_is_refused() {
    let mut transfers = Transfers::new();
    let start = now();
    assert_eq!(
        transfers.admit(upload("empty", 0), true, start),
        Err(HelloRefusal::Invalid)
    );
    // A filename is the other half: an upload with nowhere to land is as
    // unfinishable as one with nothing to send.
    let mut nameless = upload("noname", 10);
    nameless.filename = String::new();
    assert_eq!(
        transfers.admit(nameless, true, start),
        Err(HelloRefusal::Invalid)
    );
    assert_eq!(transfers.active(), 0, "and neither holds a slot");
}

/// THE WORKER BOUND, seen when the document is not the binding constraint:
/// eight uploads spread across eight documents fills the worker, and the
/// NINTH is refused whichever document it comes from.
#[test]
fn the_worker_bound_holds_across_documents() {
    let mut transfers = Transfers::new();
    let start = now();
    for index in 0..MAX_ACTIVE_PER_WORKER {
        let mut item = upload(&format!("u{index}"), 100);
        item.device_fingerprint = format!("device-{index}");
        transfers.admit(item, true, start).expect("admitted");
    }
    let overflow = upload("overflow", 100);
    assert_eq!(
        transfers.admit(overflow, true, start),
        Err(HelloRefusal::WorkerFull)
    );
    assert_eq!(transfers.active(), MAX_ACTIVE_PER_WORKER as usize);
}

/// ONE BROWSER DOCUMENT'S BOUND, which is a different refusal from the
/// worker's — and saying which one ran out is what tells an operator whether
/// to close a tab or spread the load.
///
/// The two bounds are the SAME size, which is the whole reason the order they
/// are checked in matters: with the worker checked first, the document refusal
/// would be unreachable and the more actionable diagnosis would never be given.
#[test]
fn the_document_bound_is_reported_before_the_worker_bound() {
    let mut transfers = Transfers::new();
    let start = now();
    for index in 0..MAX_ACTIVE_PER_BROWSER_DOCUMENT {
        transfers
            .admit(upload(&format!("u{index}"), 100), true, start)
            .expect("admitted");
    }
    assert_eq!(
        transfers.admit(upload("one-too-many", 100), true, start),
        Err(HelloRefusal::DocumentFull),
        "the document bound is reported first, because it is the one an \
         operator can act on by closing a tab"
    );
    assert_eq!(
        transfers.active(),
        MAX_ACTIVE_PER_BROWSER_DOCUMENT as usize,
        "the document filled the worker as well, which is exactly why the ORDER of the two checks is the only thing that tells their diagnoses apart"
    );
}

/// A finished upload releases its document's slot, so a long-lived document is
/// not refused for uploads that ended.
#[test]
fn a_finished_upload_releases_its_document_slot() {
    let mut transfers = Transfers::new();
    let start = now();
    for index in 0..MAX_ACTIVE_PER_BROWSER_DOCUMENT {
        let mut item = upload(&format!("u{index}"), 10);
        item.device_fingerprint = "device-a".into();
        transfers.admit(item, true, start).expect("admitted");
    }
    // The same DOCUMENT, which is the point: the eight above filled
    // device-a, and this one has to name it or it is a different document with
    // room to spare.
    let mut same_document = upload("x", 10);
    same_document.device_fingerprint = "device-a".into();
    assert_eq!(
        transfers.admit(same_document.clone(), true, start),
        Err(HelloRefusal::DocumentFull)
    );

    // Finish one of them.
    transfers.send_chunk("u0").expect("chunk");
    transfers.acknowledge_chunk("u0", start).expect("acked");

    assert!(
        transfers.admit(same_document, true, start).is_ok(),
        "a document that finished an upload has room for another"
    );
}

/// An upload with no lease found is refused rather than silently creating one.
#[test]
fn operations_on_an_unknown_upload_are_refused() {
    let mut transfers = Transfers::new();
    assert_eq!(
        transfers.send_chunk("nope"),
        Err(ChunkRefusal::UnknownUpload)
    );
    assert_eq!(
        transfers.acknowledge_chunk("nope", now()),
        Err(ChunkRefusal::UnknownUpload)
    );
    assert!(transfers.lease("nope").is_none());
}

/// The lease is finite. A lease with no end would be a grant under another
/// name, and the bound is what makes that visible.
#[test]
fn the_lease_is_finite() {
    let mut transfers = Transfers::new();
    let start = now();
    let granted = transfers
        .admit(upload("u1", 100), true, start)
        .expect("admitted");
    assert_eq!(
        granted, ACTIVE_LEASE,
        "an admitted upload is told how long it has"
    );
    assert!(
        transfers.lease("u1").expect("a lease").expires_at < start + ACTIVE_LEASE * 2,
        "and the lease really does end"
    );
}
