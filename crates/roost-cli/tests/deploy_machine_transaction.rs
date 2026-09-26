//! Whether a machine transaction can be OBSERVED while it is held.
//!
//! This is the property the whole command rests on, and it is not a detail.
//! `roost deploy` takes the machine transaction in one process over one ssh
//! connection and runs the apply in a second process over a second connection.
//! The apply's first act is to refuse unless a transaction is held. If the
//! record is written inside an uncommitted database transaction — which is what
//! the first implementation did — then it is invisible to every other
//! connection, and the apply can neither read the row nor connect. Every deploy
//! was refused with "no machine transaction is held" at exactly the moment one
//! was held.
//!
//! The fix separates the two jobs the original conflated: the record is
//! COMMITTED so it can be read, and a `flock` on a separate file is the kernel
//! lock that actually serialises the machine. These tests hold a transaction in
//! one connection and read it from another, which is the deploy's shape without
//! needing ssh.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use roost_cli::deploy::machine_txn::{
    MachineTransaction, TransactionError, TransactionKind, active_transaction, gate_path,
};
use roost_cli::wall_clock;

fn scratch(label: &str) -> PathBuf {
    let path =
        std::env::temp_dir().join(format!("roost-machine-txn-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

/// The property. A transaction held in one connection is READABLE from another,
/// which is the only way the apply on the far side can satisfy its own guard.
#[tokio::test]
async fn a_held_transaction_is_readable_from_another_connection() {
    let dir = scratch("readable");
    let lock_file = dir.join("machine-transaction.sqlite");
    let journal = dir.join("deploy-journal.json");
    std::fs::write(&journal, b"{}").unwrap();

    let held = MachineTransaction::acquire(
        &lock_file,
        TransactionKind::Deploy,
        &journal,
        wall_clock::now_ms(),
    )
    .await
    .expect("a first take succeeds");

    let observed = active_transaction(&lock_file)
        .await
        .expect("the record must be readable while the transaction is held")
        .expect("a held transaction must be observable");
    assert_eq!(observed.kind, TransactionKind::Deploy);
    assert_eq!(observed.journal_path, journal.display().to_string());
    assert_eq!(observed.owner_pid, std::process::id() as i32);

    held.release().await.expect("release");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Once released, the machine is idle and says so — a record that outlived its
/// lock would make the next apply believe a machine is busy when it is not.
#[tokio::test]
async fn a_released_transaction_leaves_the_machine_idle() {
    let dir = scratch("released");
    let lock_file = dir.join("machine-transaction.sqlite");
    let journal = dir.join("deploy-journal.json");
    std::fs::write(&journal, b"{}").unwrap();

    let held = MachineTransaction::acquire(
        &lock_file,
        TransactionKind::Deploy,
        &journal,
        wall_clock::now_ms(),
    )
    .await
    .expect("a first take succeeds");
    assert!(
        active_transaction(&lock_file).await.unwrap().is_some(),
        "held before the release"
    );
    held.release().await.expect("release");
    assert!(
        active_transaction(&lock_file).await.unwrap().is_none(),
        "a released transaction is not still observed as held"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A second take is refused while the first holds the machine, and the refusal
/// names the holder rather than arriving as an opaque database error.
#[tokio::test]
async fn a_second_take_is_refused_while_the_machine_is_held() {
    let dir = scratch("second");
    let lock_file = dir.join("machine-transaction.sqlite");
    let journal = dir.join("deploy-journal.json");
    std::fs::write(&journal, b"{}").unwrap();

    let held = MachineTransaction::acquire(
        &lock_file,
        TransactionKind::Deploy,
        &journal,
        wall_clock::now_ms(),
    )
    .await
    .expect("a first take succeeds");

    let second = MachineTransaction::acquire(
        &lock_file,
        TransactionKind::Deploy,
        &journal,
        wall_clock::now_ms(),
    )
    .await;
    match second {
        Err(TransactionError::Busy { .. }) => {}
        Err(other) => panic!("a contended take must be Busy, got: {other}"),
        Ok(_) => panic!("a second take must not succeed while the machine is held"),
    }
    // The first holder is untouched by the refused take.
    assert!(
        active_transaction(&lock_file).await.unwrap().is_some(),
        "the refused take must not have released the holder"
    );
    held.release().await.expect("release");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The machine is free the instant the holder goes away, and a later take
/// succeeds over the record it left. This is the recovery a deploy depends on
/// after a deploying box is killed mid-deploy, and it is the property the
/// kernel lock exists to provide — a database transaction would have needed
/// somebody to notice the death and clean up.
#[tokio::test]
async fn a_holder_that_goes_away_leaves_the_machine_free() {
    let dir = scratch("dropped");
    let lock_file = dir.join("machine-transaction.sqlite");
    let journal = dir.join("deploy-journal.json");
    std::fs::write(&journal, b"{}").unwrap();

    // Dropped without `release`: the shape of a holder process that died.
    drop(
        MachineTransaction::acquire(
            &lock_file,
            TransactionKind::Deploy,
            &journal,
            wall_clock::now_ms(),
        )
        .await
        .expect("a first take succeeds"),
    );

    let next = MachineTransaction::acquire(
        &lock_file,
        TransactionKind::Deploy,
        &journal,
        wall_clock::now_ms(),
    )
    .await;
    assert!(
        next.is_ok(),
        "the machine is free once the holder is gone, with no cleanup step: {:?}",
        next.err()
    );
    let observed = active_transaction(&lock_file).await.unwrap();
    assert!(
        observed.is_some(),
        "the new holder's record is the one that is observed"
    );
    next.unwrap().release().await.expect("release");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The kernel lock lives in its own file, not in the database it guards: that
/// separation is the fix, so it is asserted rather than assumed.
#[tokio::test]
async fn the_kernel_lock_is_a_separate_file_from_the_record() {
    let dir = scratch("gate");
    let lock_file = dir.join("machine-transaction.sqlite");
    let journal = dir.join("deploy-journal.json");
    std::fs::write(&journal, b"{}").unwrap();
    let gate = gate_path(&lock_file);

    assert_ne!(gate, lock_file, "the gate is not the database");
    assert!(gate.starts_with(&lock_file), "and it sits beside it");
    assert!(
        !gate.exists(),
        "and it is not created until somebody takes the machine"
    );

    let held = MachineTransaction::acquire(
        &lock_file,
        TransactionKind::Deploy,
        &journal,
        wall_clock::now_ms(),
    )
    .await
    .expect("a first take succeeds");
    assert!(gate.is_file(), "taking the machine creates the gate");
    held.release().await.expect("release");
    let _ = std::fs::remove_dir_all(&dir);
}
