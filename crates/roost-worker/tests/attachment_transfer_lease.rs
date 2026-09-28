//! One admitted port's finite authority lease: it expires exactly at the idle
//! boundary, valid activity refreshes idle, and nothing extends the hard
//! lifetime. Ports v2
//! `apps/worker/tests/attachments/attachment-transfer-lease.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]


use std::time::{Duration, Instant};

use roost_protocol::attachment_transfer::{ACTIVE_MAX_MS, IDLE_MS};
use roost_worker::attachments::transfer_lease::AttachmentTransferLease;

fn at(origin: Instant, millis: u64) -> Instant {
    origin + Duration::from_millis(millis)
}

#[test]
fn the_lease_expires_exactly_at_the_idle_boundary_and_stays_expired() {
    let origin = Instant::now();
    let mut lease = AttachmentTransferLease::start(origin);
    assert_eq!(lease.deadline(), at(origin, IDLE_MS));
    assert!(lease.allows_activity(at(origin, IDLE_MS - 1)));
    assert!(!lease.allows_activity(at(origin, IDLE_MS)));
    assert!(
        !lease.note_valid_activity(at(origin, IDLE_MS)),
        "expiry latches"
    );
}

#[test]
fn valid_activity_refreshes_idle_but_never_extends_the_hard_lease() {
    let origin = Instant::now();
    let mut lease = AttachmentTransferLease::start(origin);
    let mut now = IDLE_MS - 1;
    while now < ACTIVE_MAX_MS {
        assert!(
            lease.note_valid_activity(at(origin, now)),
            "activity at {now} ms keeps the port alive"
        );
        now += IDLE_MS - 1;
    }
    assert_eq!(lease.deadline(), at(origin, ACTIVE_MAX_MS));
    assert!(!lease.note_valid_activity(at(origin, ACTIVE_MAX_MS)));
}
