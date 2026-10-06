//! The audited-input pump: FIFO batches of at most 64 rows, no outcome reported
//! before its row is durable, all 1,024 slots held through a failed write, and
//! a failed write turning each outcome into what it can still truthfully be.
//! The single pooled connection is held to stand in for v2's commit gate.
//! Ports the "input audit pump" cases of `apps/coord/tests/input-audit-batching.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;
mod terminal_input_support;

use roost_coord::terminal_input::input_audit::{INPUT_AUDIT_BATCH_MAX, INPUT_AUDIT_QUEUE_CAP};
use roost_coord::terminal_input::input_control::{
    InputControlCommand, MAX_INPUT_BYTES, process_input_control,
};
use roost_coord::terminal_input::write_control::{TerminalWriteControlResult, TerminalWriteStatus};
use terminal_input_support::{InputHarness, batch};
use tokio::task::JoinHandle;

/// A Sync-shaped audited batch from its own sender; empty data settles
/// accepted without a worker, oversized data settles rejected.
fn audited(label: &str, index: usize, oversized: bool) -> InputControlCommand {
    let data = if oversized {
        vec![0; MAX_INPUT_BYTES + 1]
    } else {
        Vec::new()
    };
    let mut command = batch(
        &format!("{label}-tab-{index}"),
        "unused",
        index as u64 + 1,
        &data,
    );
    command.identity.caller_fingerprint = format!("{label}-caller-{index}");
    command.audited = true;
    command
}

fn start(
    harness: &InputHarness,
    label: &str,
    range: std::ops::Range<usize>,
    oversized: Option<usize>,
) -> Vec<JoinHandle<TerminalWriteControlResult>> {
    range
        .map(|index| {
            let command = audited(label, index, oversized == Some(index));
            tokio::spawn(process_input_control(&harness.services, command))
        })
        .collect()
}

/// Fail every audit write whose batch carries `caller_fp`.
async fn fail_batches_carrying(harness: &InputHarness, caller_fp: &str) {
    db_support::install_refusing_trigger(
        &harness.services.db,
        "fail_audit",
        "INSERT",
        "audit_log",
        &format!("NEW.caller_fp = '{caller_fp}'"),
        "injected audit transaction failure",
    )
    .await;
}

async fn yield_to_tasks() {
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
}

// v2: "commits same-database FIFO prefixes of 64 before moving to another database"
#[tokio::test]
async fn audits_commit_in_fifo_batches_of_at_most_64() {
    let harness = InputHarness::new("audit-prefix").await;
    fail_batches_carrying(&harness, "prefix-caller-64").await;
    let held = db_support::hold_every_connection(&harness.services.db).await;
    let requests = start(&harness, "prefix", 0..66, None);
    yield_to_tasks().await;
    assert!(
        requests.iter().all(|request| !request.is_finished()),
        "no outcome precedes its row"
    );

    drop(held);
    let mut outcomes = Vec::new();
    for request in requests {
        outcomes.push(request.await.unwrap());
    }

    let statuses: Vec<TerminalWriteStatus> =
        outcomes.iter().map(|outcome| outcome.status).collect();
    let mut expected = vec![TerminalWriteStatus::Accepted; INPUT_AUDIT_BATCH_MAX];
    expected.extend([TerminalWriteStatus::Ambiguous; 2]);
    assert_eq!(
        statuses, expected,
        "only the batch holding caller 64 failed, and it held 65 too"
    );
    assert!(
        outcomes[64]
            .reason
            .contains("input audit persistence failed")
    );
    let callers: Vec<String> = sqlx::query_scalar("SELECT caller_fp FROM audit_log ORDER BY id")
        .fetch_all(harness.services.db.pool())
        .await
        .unwrap();
    let first_prefix: Vec<String> = (0..64)
        .map(|index| format!("prefix-caller-{index}"))
        .collect();
    assert_eq!(callers, first_prefix);
    let paths: Vec<String> = sqlx::query_scalar("SELECT DISTINCT path FROM audit_log")
        .fetch_all(harness.services.db.pool())
        .await
        .unwrap();
    assert_eq!(
        paths,
        vec!["/ws/coord-sync/input/accepted/0/SessionsInput".to_owned()]
    );
}

// v2: "holds all 1,024 audit slots through rollback and preserves rejected and ambiguous outcomes"
#[tokio::test]
async fn a_failed_batch_holds_its_slots_until_it_settles_and_keeps_rejections_rejected() {
    let harness = InputHarness::new("audit-capacity").await;
    fail_batches_carrying(&harness, "capacity-caller-0").await;
    let mut held = harness.services.db.pool().acquire().await.unwrap();
    let mut requests = start(&harness, "capacity", 0..INPUT_AUDIT_QUEUE_CAP, Some(0));
    yield_to_tasks().await;
    requests.extend(start(
        &harness,
        "capacity",
        INPUT_AUDIT_QUEUE_CAP..INPUT_AUDIT_QUEUE_CAP + 1,
        None,
    ));
    yield_to_tasks().await;
    assert!(
        requests.iter().all(|request| !request.is_finished()),
        "no outcome precedes its row"
    );
    let uncommitted: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    assert_eq!(uncommitted, 0);

    drop(held);
    let mut outcomes = Vec::new();
    for request in requests {
        outcomes.push(request.await.unwrap());
    }

    let failed: Vec<TerminalWriteStatus> = outcomes[..64]
        .iter()
        .map(|outcome| outcome.status)
        .collect();
    let mut expected = vec![TerminalWriteStatus::Rejected];
    expected.extend([TerminalWriteStatus::Ambiguous; 63]);
    assert_eq!(failed, expected);
    assert!(
        outcomes[0]
            .reason
            .contains("input audit persistence failed")
    );
    assert!(
        outcomes[1]
            .reason
            .contains("input audit persistence failed")
    );
    assert!(
        outcomes[64..]
            .iter()
            .all(|outcome| outcome.status == TerminalWriteStatus::Accepted)
    );
    let persisted: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(harness.services.db.pool())
        .await
        .unwrap();
    assert_eq!(persisted, 961);
}
