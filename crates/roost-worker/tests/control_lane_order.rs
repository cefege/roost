//! The two ordering lanes, tested for the property each one exists to provide:
//! the control lane never lets two grid transactions overlap, and the admission
//! lane never lets two keeper writes land out of order. Both are tested by
//! running the writers concurrently and looking at the peak, because an ordering
//! bug that only shows up under contention is the whole reason the lane is here.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use roost_protocol::wire::brand::ChannelId;
use roost_worker::session::control_lanes::{ControlKind, ControlLanes, LaneSnapshot};
use roost_worker::session::keeper_admission::{
    Admission, AdmissionKind, KEEPER_UPDATE_WRITE_REFUSAL,
};

fn channel() -> ChannelId {
    7i64.try_into().expect("a positive id is a channel id")
}

fn granted(admission: Admission) -> roost_worker::session::keeper_admission::AdmissionTicket {
    match admission {
        Admission::Granted(ticket) => ticket,
        Admission::Refused(reason) => panic!("this write must be admitted: {reason}"),
    }
}

/// Take a slot, hold it for a beat, and count how many holders there were.
async fn hold_admission(lanes: Arc<ControlLanes>, live: Arc<AtomicUsize>, peak: Arc<AtomicUsize>) {
    let ticket = granted(lanes.admit(channel(), AdmissionKind::TerminalInput));
    ticket.granted().await;
    let holding = live.fetch_add(1, Ordering::AcqRel) + 1;
    peak.fetch_max(holding, Ordering::AcqRel);
    tokio::time::sleep(Duration::from_millis(5)).await;
    live.fetch_sub(1, Ordering::AcqRel);
    // Released at the ordering boundary AND again by Drop, which is the
    // double-release a transaction actually performs. A release that is not
    // idempotent hands the permit on twice, and two keeper writes then overlap
    // for exactly as long as the first one takes.
    ticket.release();
}

/// MUTUAL EXCLUSION IS THE CONTROL LANE'S WHOLE JOB. A live resize owns the
/// synchronous result-frame boundary until the existing core is aligned, so a
/// kill and a resize may not interleave — and a terminal that interleaves them
/// paints cells from two grids onto one.
#[tokio::test]
async fn two_control_transactions_never_overlap() {
    let lanes = Arc::new(ControlLanes::new());
    let live = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    for index in 0..6usize {
        let lanes = Arc::clone(&lanes);
        let live = Arc::clone(&live);
        let peak = Arc::clone(&peak);
        handles.push(tokio::spawn(async move {
            let kind = if index % 2 == 0 {
                ControlKind::TerminalStream
            } else {
                ControlKind::TerminalKill
            };
            lanes
                .enqueue(channel(), kind, || async {
                    let holding = live.fetch_add(1, Ordering::AcqRel) + 1;
                    peak.fetch_max(holding, Ordering::AcqRel);
                    tokio::time::sleep(Duration::from_millis(2)).await;
                    live.fetch_sub(1, Ordering::AcqRel);
                    index
                })
                .await
        }));
    }
    let mut completed = 0usize;
    for handle in handles {
        completed += handle.await.expect("a control transaction returns");
    }
    assert_eq!(completed, 15, "every transaction ran exactly once");
    assert_eq!(
        peak.load(Ordering::Acquire),
        1,
        "two transactions on one channel overlapped, and the grid they both \
         wrote is now two grids"
    );
    assert_eq!(
        lanes.snapshot(channel()),
        LaneSnapshot::default(),
        "an idle lane reports idle rather than a retained never-cleared record"
    );
}

/// RECEIVE ORDER IS THE ADMISSION LANE'S WHOLE JOB. A keystroke that overtakes
/// the resize ahead of it lands on the wrong geometry, which is the defect the
/// lane was added for.
#[tokio::test]
async fn two_keeper_writes_never_enter_the_lane_together() {
    let lanes = Arc::new(ControlLanes::new());
    let live = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    for _ in 0..6 {
        let lanes = Arc::clone(&lanes);
        let live = Arc::clone(&live);
        let peak = Arc::clone(&peak);
        handles.push(tokio::spawn(async move {
            hold_admission(lanes, live, peak).await;
        }));
    }
    for handle in handles {
        handle.await.expect("a keeper write returns");
    }
    assert_eq!(
        peak.load(Ordering::Acquire),
        1,
        "two keeper writes held the write-ordering lane at once, so the keeper \
         received them in an order nobody asked for"
    );
    assert_eq!(
        lanes.snapshot(channel()).admission_depth,
        0,
        "and the lane is empty again, which a release that double-decremented \
         could not report"
    );
}

/// THE GATE FAILS CLOSED. A keeper an update is about to replace must not absorb
/// one more byte: a write that reached the old keeper would be a write the
/// replacement never sees, and the client's retry would duplicate it.
#[tokio::test]
async fn a_prepared_keeper_replacement_refuses_a_terminal_write() {
    let lanes = ControlLanes::new();
    assert!(!lanes.keeper_update_prepared());
    lanes.set_keeper_update_prepared(true);
    assert!(lanes.keeper_update_prepared());

    for kind in [AdmissionKind::TerminalResize, AdmissionKind::TerminalInput] {
        let admission = lanes.admit(channel(), kind);
        assert_eq!(
            admission.refusal(),
            Some(KEEPER_UPDATE_WRITE_REFUSAL),
            "{kind:?} is refused outright, not queued behind a dying keeper"
        );
        assert!(!admission.is_granted());
    }

    // A query reply is not a terminal write: it is the core answering a probe
    // the application is already blocked on.
    let query = granted(lanes.admit(channel(), AdmissionKind::QueryReply));
    query.granted().await;
    query.release();

    lanes.set_keeper_update_prepared(false);
    let readmitted = granted(lanes.admit(channel(), AdmissionKind::TerminalInput));
    readmitted.granted().await;
    readmitted.release();
    assert!(
        !lanes.keeper_update_prepared(),
        "and clearing the gate admits writes again, or the worker has wedged"
    );
}

/// A TICKET RELEASED BEFORE IT EVER ENTERED TAKES NOTHING. A transaction that
/// discovered it was superseded on the way to the keeper releases at its
/// boundary, and re-entering would queue it behind the very writes it was meant
/// to precede.
#[tokio::test]
async fn a_ticket_released_before_it_was_granted_takes_nothing() {
    let lanes = ControlLanes::new();
    let ticket = granted(lanes.admit(channel(), AdmissionKind::TerminalInput));
    ticket.release();
    assert_eq!(
        lanes.snapshot(channel()).admission_depth,
        0,
        "a released ticket is off the lane"
    );
    ticket.granted().await;
    assert_eq!(
        lanes.snapshot(channel()).admission_holder,
        None,
        "and granting it afterwards does not put it back, which would queue it \
         behind the writes it was meant to precede"
    );
    assert_eq!(ticket.kind(), AdmissionKind::TerminalInput);
}

/// A READER WAITS FOR THE TRANSACTION IN FLIGHT. A dims-change claim rebuilds a
/// fresh core inside its control transaction, and a page served mid-rebuild
/// hands out rows the imminent reframe invalidates.
#[tokio::test]
async fn a_settled_reader_waits_for_the_running_transaction() {
    let lanes = Arc::new(ControlLanes::new());
    let running = Arc::new(AtomicUsize::new(0));
    let observed = Arc::new(AtomicUsize::new(usize::MAX));
    let writer = {
        let lanes = Arc::clone(&lanes);
        let running = Arc::clone(&running);
        async move {
            lanes
                .enqueue(channel(), ControlKind::TerminalStream, || async {
                    running.store(1, Ordering::Release);
                    tokio::time::sleep(Duration::from_millis(40)).await;
                    running.store(0, Ordering::Release);
                })
                .await
        }
    };
    let reader = {
        let lanes = Arc::clone(&lanes);
        let running = Arc::clone(&running);
        let observed = Arc::clone(&observed);
        async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            lanes.settled(channel()).await;
            observed.store(running.load(Ordering::Acquire), Ordering::Release);
        }
    };
    // Joined rather than spawned: the reader must observe the writer still
    // holding the lane, so the two have to be in flight at the same time.
    let (_writer_ran, _reader_ran) = tokio::join!(writer, reader);
    assert_eq!(
        observed.load(Ordering::Acquire),
        0,
        "the reader observed the core while a transaction still owned it, so \
         every row it read belonged to a grid that was about to be replaced"
    );
}

/// THE LANE NAMES WHAT IT IS HOLDING, because the gate is invisible from the
/// outside: a worker that quietly stops accepting terminal writes looks exactly
/// like a coordinator outage unless the transition is on the record.
#[tokio::test]
async fn a_holder_is_named_while_it_holds_and_named_nothing_after() {
    let lanes = Arc::new(ControlLanes::new());
    let ticket = granted(lanes.admit(channel(), AdmissionKind::TerminalResize));
    assert_eq!(lanes.snapshot(channel()).admission_depth, 1);
    ticket.granted().await;
    let held = lanes.snapshot(channel());
    assert_eq!(held.admission_holder, Some(AdmissionKind::TerminalResize));
    assert_eq!(held.control_running, None, "the control lane is separate");
    ticket.release();
    assert_eq!(lanes.snapshot(channel()).admission_holder, None);
}
