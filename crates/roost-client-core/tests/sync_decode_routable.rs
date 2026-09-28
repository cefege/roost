//! The chunked routable-set arm and the audit-row arm, decoded from the frames
//! `roost-coord`'s feed builds and applied through `ClientCore::handle`.
//!
//! Ported from v2 `apps/web/src/store/sync-inbound.ts:150-178`
//! (`dispatchRoutableChunk`) and `apps/web/src/store/sync-handlers.ts` (audit).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod sync_decode_support;

use roost_client_core::SyncDomain;
use roost_client_core::store::sync_feeds::AUDIT_ROW_RING_MAX;
use roost_client_core::sync::decode::DecodeRefusal;
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::{AuditRow, WorkerRoutableFrame};

use sync_decode_support::{WORKER_FP, acked, application, closes, deliver, ready_core, refused};

fn routable_arm(fps: &[&str], snapshot_id: &str, chunk_index: u32, chunk_count: u32) -> Frame {
    Frame::WorkerRoutable(Box::new(WorkerRoutableFrame {
        fps: fps.iter().map(|fp| (*fp).to_owned()).collect(),
        snapshot_id: snapshot_id.to_owned(),
        chunk_index,
        chunk_count,
        ..WorkerRoutableFrame::default()
    }))
}

fn routable(core: &roost_client_core::ClientCore) -> Option<Vec<&str>> {
    core.store()
        .routable_worker_fps
        .as_ref()
        .map(|set| set.iter().map(String::as_str).collect())
}

#[test]
fn a_live_routable_frame_replaces_the_set_wholesale() {
    let (mut core, generation) = ready_core();
    // The bootstrap workersList seeds the set (v2 `sync-routable.ts:5`); the
    // hydration fixture lists no routable worker.
    assert_eq!(
        routable(&core),
        Some(vec![]),
        "the hydrated set before any frame"
    );
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Workers, 1, routable_arm(&["a", "b"], "", 0, 0)),
    );
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Workers, 2, routable_arm(&["c"], "", 0, 0)),
    );
    assert_eq!(routable(&core), Some(vec!["c"]));
}

#[test]
fn a_chunked_seed_publishes_nothing_until_complete_then_replaces_the_set() {
    let (mut core, generation) = ready_core();
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Workers, 1, routable_arm(&["old"], "", 0, 0)),
    );
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Workers, 2, routable_arm(&["c"], "seed-1", 2, 3)),
    );
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Workers, 3, routable_arm(&["a"], "seed-1", 0, 3)),
    );
    assert_eq!(
        routable(&core),
        Some(vec!["old"]),
        "an incomplete seed publishes nothing"
    );
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Workers, 4, routable_arm(&["b"], "seed-1", 1, 3)),
    );
    assert_eq!(routable(&core), Some(vec!["a", "b", "c"]));
    assert!(!core.store().routable_assembly.is_assembling());
}

#[test]
fn a_new_seed_discards_the_old_partial_one() {
    let (mut core, generation) = ready_core();
    deliver(
        &mut core,
        generation,
        &application(
            SyncDomain::Workers,
            1,
            routable_arm(&["stale"], "seed-1", 0, 2),
        ),
    );
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Workers, 2, routable_arm(&["x"], "seed-2", 0, 2)),
    );
    // seed-1's missing chunk arriving now starts seed-1 over; it cannot
    // complete seed-1 with the chunk seed-2 displaced.
    deliver(
        &mut core,
        generation,
        &application(
            SyncDomain::Workers,
            3,
            routable_arm(&["late"], "seed-1", 1, 2),
        ),
    );
    assert_eq!(routable(&core), Some(vec![]), "still the hydrated set");
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Workers, 4, routable_arm(&["y"], "seed-2", 0, 2)),
    );
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Workers, 5, routable_arm(&["z"], "seed-2", 1, 2)),
    );
    assert_eq!(routable(&core), Some(vec!["y", "z"]));
}

#[test]
fn routable_chunk_numbering_out_of_bounds_or_disagreeing_closes_the_link() {
    for (index, count) in [(0, 0), (0, 4097), (3, 3)] {
        assert!(
            matches!(
                refused(&application(
                    SyncDomain::Workers,
                    1,
                    routable_arm(&["a"], "s", index, count)
                )),
                DecodeRefusal::MalformedArm {
                    arm: "worker_routable",
                    ..
                }
            ),
            "chunk {index} of {count}"
        );
    }
    let (mut core, generation) = ready_core();
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Workers, 1, routable_arm(&["a"], "s", 0, 2)),
    );
    let effects = deliver(
        &mut core,
        generation,
        &application(SyncDomain::Workers, 2, routable_arm(&["b"], "s", 1, 3)),
    );
    assert!(closes(&effects, generation), "{effects:?}");
    assert!(acked(&effects).is_empty());
    assert_eq!(
        routable(&core),
        Some(vec![]),
        "the disagreeing seed published nothing over the hydrated set"
    );
}

fn audit_arm(id: u64) -> Frame {
    Frame::AuditRow(Box::new(AuditRow {
        id,
        ts: 1_000 + id,
        caller_fp: Some(WORKER_FP.to_owned()),
        method: "POST".to_owned(),
        path: "/roost.v1.Coordinator/SessionsKill".to_owned(),
        status: 200,
        ..AuditRow::default()
    }))
}

#[test]
#[ignore = "SETTINGS: the audit domain is lazy; it becomes ready only through AuditLogPane's lazy hydrator (v2 registerLazySyncDomain)"]
fn audit_rows_are_newest_first_deduplicated_and_bounded() {
    let (mut core, generation) = ready_core();
    let total = AUDIT_ROW_RING_MAX as u64 + 5;
    for id in 1..=total {
        deliver(
            &mut core,
            generation,
            &application(SyncDomain::Audit, id, audit_arm(id)),
        );
    }
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Audit, total + 1, audit_arm(total)),
    );
    let rows = &core.store().audit_rows;
    assert_eq!(rows.len(), AUDIT_ROW_RING_MAX);
    assert_eq!(
        rows.front().map(|row| row.id),
        Some(total),
        "newest first, once"
    );
    assert_eq!(
        rows.back().map(|row| row.id),
        Some(6),
        "the oldest fell off"
    );
}
