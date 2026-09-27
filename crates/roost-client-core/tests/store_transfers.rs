//! Transfers: progress may go backwards, and a late tick may not rewrite a card
//! that has moved on.
//!
//! The defect this file exists to prevent is a store that reads a decreasing byte
//! count as failure: a chunked upload that reports 99% and restarts from zero is
//! succeeding, and a card that shows it failing while it works is a card the user
//! cannot act on.
//!
//! The mutation experiment for this file is in the slice report: in
//! `set_transfer_progress`, replace `transfer.state = TransferState::Running`
//! with `transfer.state = TransferState::Failed`, and
//! `a_transfer_may_decrease_without_failing` must fail.

use roost_client_core::ClientCore;
use roost_client_core::store::transfers::{
    TRANSFER_STALL_AFTER_MS, TransferDirection, TransferState, add_transfer, mark_transfer_state,
    remove_transfer, set_transfer_progress, sweep_transfers,
};

/// A client over the in-memory host.
fn client() -> ClientCore {
    ClientCore::in_memory("tab-transfers")
}

#[test]
fn a_transfer_may_decrease_without_failing() {
    let mut core = client();
    add_transfer(
        core.store_mut(),
        "upload-1",
        "big.tgz",
        TransferDirection::Up,
        1_000,
        TransferState::Running,
        None,
        0,
    );
    set_transfer_progress(core.store_mut(), "upload-1", 900, None, 1_000);
    set_transfer_progress(core.store_mut(), "upload-1", 990, None, 2_000);
    assert_eq!(
        core.store()
            .transfers
            .transfer("upload-1")
            .map(|card| card.state),
        Some(TransferState::Running)
    );
    // The restart: the chunked upload began again and the count fell to zero.
    set_transfer_progress(core.store_mut(), "upload-1", 0, None, 3_000);
    let card = core
        .store()
        .transfers
        .transfer("upload-1")
        .expect("the card is there");
    assert_eq!(
        card.state,
        TransferState::Running,
        "a decrease is a restart inside running, never a failure"
    );
    assert_eq!(card.bytes_done, 0);
    assert!(
        card.eta_s.is_none(),
        "the restarted run has no rate yet, so the ETA reports unknown rather than the old run's"
    );
    assert!(card.speed_bps.is_none());
    // And it climbs again from the restart.
    set_transfer_progress(core.store_mut(), "upload-1", 400, None, 4_000);
    let card = core
        .store()
        .transfers
        .transfer("upload-1")
        .expect("the card is there");
    assert_eq!(card.bytes_done, 400);
    assert!(card.speed_bps.unwrap_or_default() > 0.0);
    assert_eq!(card.state, TransferState::Running);
}

#[test]
fn a_late_tick_cannot_move_a_settled_card_or_resurrect_a_dismissed_one() {
    let mut core = client();
    add_transfer(
        core.store_mut(),
        "upload-1",
        "one.tgz",
        TransferDirection::Up,
        1_000,
        TransferState::Running,
        None,
        0,
    );
    set_transfer_progress(core.store_mut(), "upload-1", 1_000, None, 1_000);
    mark_transfer_state(
        core.store_mut(),
        "upload-1",
        TransferState::Done,
        None,
        1_100,
    );
    let before = core.store().revision();
    assert!(!set_transfer_progress(
        core.store_mut(),
        "upload-1",
        10,
        None,
        1_200
    ));
    assert_eq!(
        core.store().revision(),
        before,
        "a refused tick is not a change"
    );
    assert_eq!(
        core.store()
            .transfers
            .transfer("upload-1")
            .map(|card| card.bytes_done),
        Some(1_000),
        "the last tick of a finished upload must not walk its byte count back down"
    );
    assert!(!mark_transfer_state(
        core.store_mut(),
        "upload-1",
        TransferState::Failed,
        Some("late".to_owned()),
        1_300
    ));
    assert_eq!(
        core.store()
            .transfers
            .transfer("upload-1")
            .map(|card| card.state),
        Some(TransferState::Done)
    );
    assert!(remove_transfer(core.store_mut(), "upload-1"));
    assert!(!set_transfer_progress(
        core.store_mut(),
        "upload-1",
        500,
        None,
        1_400
    ));
    assert_eq!(
        core.store().transfers.len(),
        0,
        "a late callback cannot resurrect a card"
    );
}

#[test]
fn a_transfer_may_not_come_back_from_the_dead() {
    let mut core = client();
    add_transfer(
        core.store_mut(),
        "download-1",
        "notes.md",
        TransferDirection::Down,
        0,
        TransferState::Running,
        None,
        0,
    );
    mark_transfer_state(
        core.store_mut(),
        "download-1",
        TransferState::Failed,
        Some("read failed".to_owned()),
        10,
    );
    assert!(!mark_transfer_state(
        core.store_mut(),
        "download-1",
        TransferState::Running,
        None,
        20
    ));
    assert_eq!(
        core.store()
            .transfers
            .transfer("download-1")
            .map(|card| card.state),
        Some(TransferState::Failed)
    );
}

#[test]
fn a_stalled_card_is_revived_by_the_next_tick() {
    let mut core = client();
    add_transfer(
        core.store_mut(),
        "upload-1",
        "one.tgz",
        TransferDirection::Up,
        1_000,
        TransferState::Running,
        None,
        0,
    );
    set_transfer_progress(core.store_mut(), "upload-1", 100, None, 1_000);
    assert!(sweep_transfers(
        core.store_mut(),
        1_000 + TRANSFER_STALL_AFTER_MS
    ));
    assert_eq!(
        core.store()
            .transfers
            .transfer("upload-1")
            .map(|card| card.state),
        Some(TransferState::Stalled)
    );
    set_transfer_progress(core.store_mut(), "upload-1", 200, None, 17_000);
    assert_eq!(
        core.store()
            .transfers
            .transfer("upload-1")
            .map(|card| card.state),
        Some(TransferState::Running)
    );
}
