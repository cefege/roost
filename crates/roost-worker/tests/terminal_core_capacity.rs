//! Terminal-core admission over deterministic memory seams: steady-cap math,
//! the heartbeat report, refusal, replacement serialization, the survivor-set
//! gate, the operator cap parse, the cgroup ceiling, and a spawn's lease.
//! Ports `apps/worker/tests/terminal/terminal-core-capacity.test.ts` and the
//! `ROOST_WORKER_TERMINAL_CAP` cases of `apps/worker/tests/host/config.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

// This binary uses the spawn fixture's lease path only; `session_spawn` uses the rest.
#[path = "session_spawn_support/mod.rs"]
#[allow(dead_code, unused_imports)]
mod spawn_support;

use std::sync::Arc;

use roost_host::MapEnv;
use roost_host::host_memory::effective_linux_memory_ceiling_bytes;
use roost_protocol::wire::TerminalCoreCapacityReport;
use roost_worker::event_store::DurableEventKind;
use roost_worker::session::spawn::{SpawnRefusal, spawn_shell};
use roost_worker::session::types::SessionRecord;
use roost_worker::terminal_core_capacity::{
    ENV_WORKER_TERMINAL_CAP, TERMINAL_CORE_ALLOCATION_BYTES, TERMINAL_CORE_CAPACITY_ERROR_CODE,
    TERMINAL_CORE_CAPACITY_ERROR_MESSAGE, TERMINAL_CORE_CAPACITY_HARD_MAX,
    TerminalCoreAllocationKind as Kind, TerminalCoreCapacity, TerminalCoreCapacityError,
    TerminalCoreCapacityOptions, TerminalCoreCapacityRefusalReason as Reason,
    terminal_core_cap_from_env,
};

use spawn_support::{
    BindingThatRecordsDelivery, FakeKeeper, FixedResolver, LedgerSink, context, request, worker_fp,
};

const TEST_CEILING_BYTES: u64 = 1_000 * TERMINAL_CORE_ALLOCATION_BYTES;

fn capacity_with_limit(terminal_core_cap: u32) -> Arc<TerminalCoreCapacity> {
    TerminalCoreCapacity::new(TerminalCoreCapacityOptions {
        effective_memory_ceiling_bytes: TEST_CEILING_BYTES,
        boot_rss_bytes: 0,
        terminal_core_cap: Some(terminal_core_cap),
    })
}

fn assert_refused(
    outcome: Result<impl std::fmt::Debug, TerminalCoreCapacityError>,
    reason: Reason,
) {
    let refusal = outcome.expect_err("capacity must refuse");
    assert_eq!(refusal.reason, reason);
    assert_eq!(refusal.code(), TERMINAL_CORE_CAPACITY_ERROR_CODE);
    assert_eq!(refusal.to_string(), TERMINAL_CORE_CAPACITY_ERROR_MESSAGE);
}

fn used_pending(capacity: &TerminalCoreCapacity) -> (u32, u32) {
    let snapshot = capacity.snapshot();
    (snapshot.used, snapshot.pending)
}

#[test]
fn the_steady_cap_is_host_derived_bounded_by_the_operator_and_the_hard_max() {
    let ceiling = 35 * TERMINAL_CORE_ALLOCATION_BYTES;
    let boot_rss = 20 * TERMINAL_CORE_ALLOCATION_BYTES;
    let capacity = TerminalCoreCapacity::new(TerminalCoreCapacityOptions {
        effective_memory_ceiling_bytes: ceiling,
        boot_rss_bytes: boot_rss,
        terminal_core_cap: None,
    });
    assert_eq!(
        capacity.snapshot(),
        TerminalCoreCapacityReport {
            used: 0,
            pending: 0,
            capacity: 3,
            estimated_reserved_bytes: 0,
            effective_memory_ceiling_bytes: ceiling,
            boot_rss_bytes: boot_rss,
            overcommit_count: 0,
            refusal_count: 0,
        }
    );
    assert_eq!(capacity_with_limit(2).snapshot().capacity, 2);
    let high_operator_cap = TerminalCoreCapacity::new(TerminalCoreCapacityOptions {
        effective_memory_ceiling_bytes: 11 * TERMINAL_CORE_ALLOCATION_BYTES,
        boot_rss_bytes: 0,
        terminal_core_cap: Some(99),
    });
    assert_eq!(
        high_operator_cap.snapshot().capacity,
        6,
        "an operator cap never raises the host cap"
    );
    let hard_capped = TerminalCoreCapacity::new(TerminalCoreCapacityOptions {
        effective_memory_ceiling_bytes: TEST_CEILING_BYTES,
        boot_rss_bytes: 0,
        terminal_core_cap: None,
    });
    assert_eq!(
        hard_capped.snapshot().capacity,
        TERMINAL_CORE_CAPACITY_HARD_MAX
    );
}

#[test]
fn the_ceiling_is_the_lowest_finite_cgroup_limit_else_host_memory() {
    let cgroup = |high: &'static str, max: &'static str| {
        move |path: &str| -> Option<String> {
            match path {
                "/proc/self/cgroup" => Some("0::/roost-worker\n".to_owned()),
                "/sys/fs/cgroup/roost-worker/memory.high" => Some(high.to_owned()),
                "/sys/fs/cgroup/roost-worker/memory.max" => Some(max.to_owned()),
                other => panic!("unexpected cgroup path {other}"),
            }
        }
    };
    assert_eq!(
        effective_linux_memory_ceiling_bytes(800, &cgroup("600", "700")),
        600
    );
    assert_eq!(
        effective_linux_memory_ceiling_bytes(800, &cgroup("max", "max")),
        800
    );
    let root = |path: &str| -> Option<String> {
        match path {
            "/proc/self/cgroup" => Some("0::/\n".to_owned()),
            "/sys/fs/cgroup/memory.high" => Some("max".to_owned()),
            "/sys/fs/cgroup/memory.max" => Some("512".to_owned()),
            other => panic!("unexpected root cgroup path {other}"),
        }
    };
    assert_eq!(effective_linux_memory_ceiling_bytes(800, &root), 512);
    let unreadable = |_: &str| -> Option<String> { None };
    assert_eq!(effective_linux_memory_ceiling_bytes(800, &unreadable), 800);
}

#[test]
fn fresh_and_adoption_are_refused_at_zero_or_full_capacity() {
    let zero = capacity_with_limit(0);
    assert_refused(zero.reserve(Kind::Fresh), Reason::Allocation(Kind::Fresh));
    assert_refused(
        zero.reserve(Kind::Adoption),
        Reason::Allocation(Kind::Adoption),
    );
    assert_eq!(zero.snapshot().refusal_count, 2);

    let full = capacity_with_limit(1);
    let lease = full.reserve(Kind::Fresh).expect("one slot is free");
    lease.activate().expect("a fresh lease activates once");
    assert_refused(full.reserve(Kind::Fresh), Reason::Allocation(Kind::Fresh));
    assert_refused(
        full.reserve(Kind::Adoption),
        Reason::Allocation(Kind::Adoption),
    );
    let snapshot = full.snapshot();
    assert_eq!(
        (
            snapshot.used,
            snapshot.pending,
            snapshot.capacity,
            snapshot.refusal_count
        ),
        (1, 0, 1, 2)
    );
    lease.release();
    assert_eq!(used_pending(&full), (0, 0));
}

#[test]
fn replacement_headroom_is_serialized_until_the_old_record_is_torn_down() {
    let capacity = capacity_with_limit(1);
    let old_lease = capacity.reserve(Kind::Fresh).unwrap();
    old_lease.activate().unwrap();
    let replacement = capacity
        .reserve(Kind::Replacement)
        .expect("one slot of overcommit");
    let snapshot = capacity.snapshot();
    assert_eq!(
        (snapshot.used, snapshot.pending, snapshot.overcommit_count),
        (1, 1, 1)
    );
    assert_refused(
        capacity.reserve(Kind::Replacement),
        Reason::Allocation(Kind::Replacement),
    );

    replacement.activate().unwrap();
    old_lease.release();
    let snapshot = capacity.snapshot();
    assert_eq!(
        (snapshot.used, snapshot.pending, snapshot.overcommit_count),
        (1, 0, 0)
    );
    assert_refused(
        capacity.reserve(Kind::Replacement),
        Reason::Allocation(Kind::Replacement),
    );
    capacity
        .complete_replacement(&replacement)
        .expect("the resident replacement completes");

    let next = capacity
        .reserve(Kind::Replacement)
        .expect("the slot was freed");
    assert!(
        capacity.complete_replacement(&next).is_err(),
        "a pending lease cannot complete"
    );
    next.release();
    replacement.release();
    assert_eq!(used_pending(&capacity), (0, 0));
}

#[test]
fn an_over_cap_survivor_set_is_refused_before_any_lease_is_taken() {
    let capacity = capacity_with_limit(2);
    let resident = capacity.reserve(Kind::Fresh).unwrap();
    resident.activate().unwrap();
    assert_refused(
        capacity.assert_can_adopt_survivors(2),
        Reason::SurvivorCount,
    );
    assert_eq!(used_pending(&capacity), (1, 0), "the refusal took no lease");
    assert_eq!(capacity.snapshot().refusal_count, 1);
    capacity
        .assert_can_adopt_survivors(1)
        .expect("one more survivor fits");
}

#[test]
fn the_operator_cap_accepts_only_a_strict_nonnegative_decimal() {
    let cap = |value: Option<&str>| {
        let env = match value {
            Some(value) => MapEnv::new().with(ENV_WORKER_TERMINAL_CAP, value),
            None => MapEnv::new(),
        };
        terminal_core_cap_from_env(&env)
    };
    assert_eq!(cap(None), Ok(None));
    assert_eq!(cap(Some("0")), Ok(Some(0)));
    assert_eq!(cap(Some("17")), Ok(Some(17)));
    assert_eq!(cap(Some("4294967295")), Ok(Some(u32::MAX)));
    for refused in [
        "",
        "-1",
        "+1",
        "01",
        "1.5",
        " 1",
        "1 ",
        "1e2",
        "4294967296",
        "9007199254740992",
    ] {
        assert!(cap(Some(refused)).is_err(), "{refused:?} was accepted");
    }
}

/// One spawn through the real `spawn_shell`, with both claims it consumes.
async fn spawn_through(
    keeper: &Arc<FakeKeeper>,
    events: &Arc<LedgerSink>,
    capacity: &TerminalCoreCapacity,
    channel_id: i64,
) -> (Result<SessionRecord, SpawnRefusal>, [u64; 2]) {
    let resolver = FixedResolver::at("/tmp");
    let fp = worker_fp();
    let opened = events.reserve(DurableEventKind::Opened);
    let close = events.reserve(DurableEventKind::Closed);
    let spawned = spawn_shell(
        &context(keeper, events, &resolver, &fp, capacity),
        opened,
        close,
        Arc::new(BindingThatRecordsDelivery),
        request(channel_id),
        1_000,
    )
    .await;
    (spawned, [opened.id(), close.id()])
}

#[tokio::test]
async fn a_spawn_holds_its_lease_resident_and_a_refused_spawn_gives_it_back() {
    let capacity = capacity_with_limit(1);
    let events = LedgerSink::new();

    let (refused, _) = spawn_through(&FakeKeeper::refusing(), &events, &capacity, 31).await;
    assert!(matches!(refused, Err(SpawnRefusal::KeeperRefused { .. })));
    assert_eq!(
        used_pending(&capacity),
        (0, 0),
        "a spawn the keeper refused kept its core slot"
    );

    let working = FakeKeeper::working();
    let (spawned, _) = spawn_through(&working, &events, &capacity, 32).await;
    spawned.expect("a working keeper spawns");
    assert_eq!(
        used_pending(&capacity),
        (1, 0),
        "the spawned record's core is resident"
    );

    let (over, claims) = spawn_through(&working, &events, &capacity, 33).await;
    assert!(matches!(over, Err(SpawnRefusal::TerminalCoreCapacity(_))));
    assert_eq!(
        working.opened_channels().len(),
        1,
        "an over-cap spawn opened a PTY"
    );
    assert!(
        claims.iter().all(|claim| events.released().contains(claim)),
        "an over-cap spawn kept a claim"
    );

    assert!(
        capacity.release_channel(32),
        "the spawned channel's lease is released at teardown"
    );
    assert_eq!(used_pending(&capacity), (0, 0));
}
