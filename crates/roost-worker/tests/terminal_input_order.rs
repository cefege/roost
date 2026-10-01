//! The write-ordering lane is a CONTRACT about order, and it is the one
//! property of that lane no counter can check.
//!
//! Fast-typed input used to reach the PTY TRANSPOSED — `BRACKETLESS` painted as
//! `BRACKETELSS`, `WB_NARROW_` as `BW_NARROW_` — with nothing lost. Every
//! count in the worker was correct, which is why it survived: the lane handed
//! out a semaphore permit, and a semaphore grants in the order its waiters are
//! POLLED, not the order the writes were admitted in. On a multi-threaded
//! runtime those differ, and adjacent keystrokes went in backwards.
//!
//! So the test admits N writes in a known order, lets each take the lane and
//! yield so the scheduler is free to interleave them, and asserts the order they
//! actually got. Anything less than the full order assertion passes against a
//! lane that reorders within a group.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use roost_protocol::wire::brand::ChannelId;
use roost_worker::session::control_lanes::ControlLanes;
use roost_worker::session::keeper_admission::{Admission, AdmissionKind};

const WRITES: usize = 400;

/// THE LANE GRANTS IN ADMIT ORDER, under exactly the conditions that broke it:
/// a multi-threaded runtime, and a yield inside the lane so every admitted
/// writer is pollable before the one in front of it has released.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn the_write_ordering_lane_grants_in_admit_order() {
    let lanes = Arc::new(ControlLanes::new());
    let order = Arc::new(Mutex::new(Vec::new()));
    let mut tasks = Vec::new();
    for index in 0..WRITES {
        let Admission::Granted(ticket) = lanes.admit(
            ChannelId::try_from(1i64).expect("a positive id is a channel id"),
            AdmissionKind::TerminalInput,
        ) else {
            panic!("the lane refuses nothing while no keeper update is prepared")
        };
        let order = Arc::clone(&order);
        tasks.push(tokio::spawn(async move {
            ticket.granted().await;
            order.lock().unwrap().push(index);
            // The write itself: the moment the real path is on the keeper
            // socket. Yielding here is what gives the scheduler the opportunity
            // to poll the NEXT admitted writer before this one lets go.
            tokio::task::yield_now().await;
            ticket.release();
        }));
    }
    for task in tasks {
        task.await.expect("no admitted write panicked");
    }

    let granted = order.lock().unwrap().clone();
    assert_eq!(
        granted.len(),
        WRITES,
        "every admitted write entered the lane exactly once"
    );
    let inversions = granted.windows(2).filter(|pair| pair[0] > pair[1]).count();
    assert_eq!(
        inversions, 0,
        "the write-ordering lane reordered keeper writes: {inversions} inversions in {granted:?}"
    );
}

/// A ticket released BEFORE it ever entered must not pass the lane past its
/// successor: it was never ahead of anyone, and a hand-off from a writer that
/// never wrote is how two writes end up overlapping.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn a_ticket_released_before_it_writes_does_not_pass_the_lane_on() {
    let lanes = Arc::new(ControlLanes::new());
    let channel = ChannelId::try_from(1i64).expect("a positive id is a channel id");

    let Admission::Granted(first) = lanes.admit(channel, AdmissionKind::TerminalInput) else {
        panic!("an idle lane grants")
    };
    first.granted().await;

    // Admitted behind the holder, then released without ever entering.
    let Admission::Granted(refused) = lanes.admit(channel, AdmissionKind::TerminalInput) else {
        panic!("a held lane still admits; it queues")
    };
    refused.release();

    // The next writer must still be BEHIND the holder, not run beside it.
    let Admission::Granted(next) = lanes.admit(channel, AdmissionKind::TerminalInput) else {
        panic!("a held lane still admits")
    };
    let entered = Arc::new(Mutex::new(false));
    let flag = Arc::clone(&entered);
    let waiter = tokio::spawn(async move {
        next.granted().await;
        *flag.lock().unwrap() = true;
        next.release();
    });

    tokio::task::yield_now().await;
    assert!(
        !*entered.lock().unwrap(),
        "a writer stays out while the lane is held, even after the ticket ahead of \
         it was released without writing"
    );
    first.release();
    waiter.await.expect("the waiter finished");
    assert!(
        *entered.lock().unwrap(),
        "and it enters once the lane is free"
    );
}

/// THE IDLE GRANT IS A HOLD, NOT A PLACE IN A QUEUE, and giving it back has to
/// hand the lane on. An agent prompt is admitted, its process proof is
/// refreshed while the grant is still pending, and the refresh can decide the
/// prompt is not ours — so a writer that never entered releases a lane it has
/// held since it was issued. If that release only gives up a QUEUE place, there
/// is no place to give up, the hold is never handed on, and every later keeper
/// write on the channel waits for a hand-off that cannot come.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn a_writer_released_before_it_enters_hands_the_idle_lane_on() {
    let lanes = ControlLanes::new();
    let channel = ChannelId::try_from(2i64).expect("a positive id is a channel id");

    let Admission::Granted(refused_prompt) = lanes.admit(channel, AdmissionKind::TerminalInput)
    else {
        panic!("an idle lane grants at once")
    };
    // Refused after admission, before the grant was ever awaited.
    refused_prompt.release();

    let Admission::Granted(keystroke) = lanes.admit(channel, AdmissionKind::TerminalInput) else {
        panic!("a lane with no holder still admits")
    };
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(5), keystroke.granted())
            .await
            .is_ok(),
        "a write refused before it entered must not strand the channel: the next \
         keeper write waited for a hand-off nobody was left to give"
    );
}

/// The same refusal, with a write already queued behind it: the queue is not
/// drained by the refusal either, so the writer behind it is the one that has
/// to be woken.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn a_writer_queued_behind_a_refused_one_still_enters() {
    let lanes = ControlLanes::new();
    let channel = ChannelId::try_from(3i64).expect("a positive id is a channel id");

    let Admission::Granted(refused_prompt) = lanes.admit(channel, AdmissionKind::TerminalInput)
    else {
        panic!("an idle lane grants at once")
    };
    let Admission::Granted(queued) = lanes.admit(channel, AdmissionKind::TerminalInput) else {
        panic!("a held lane still admits; it queues")
    };
    refused_prompt.release();

    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(5), queued.granted())
            .await
            .is_ok(),
        "the write behind a writer that was refused before it entered never got in"
    );
}

/// A burst where writers are refused mid-lane, which is what a burst of agent
/// prompts against a busy channel looks like. Every write that did not refuse
/// must still enter, in admit order, and the lane must not overlap two of them.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn a_burst_of_refusals_neither_strands_nor_reorders_the_lane() {
    const WRITERS: usize = 60;

    let lanes = Arc::new(ControlLanes::new());
    let channel = ChannelId::try_from(4i64).expect("a positive id is a channel id");
    let entered = Arc::new(Mutex::new(Vec::new()));
    let inside = Arc::new(Mutex::new(0usize));
    let mut tasks = Vec::new();
    for index in 0..WRITERS {
        let Admission::Granted(ticket) = lanes.admit(channel, AdmissionKind::TerminalInput) else {
            panic!("the lane refuses nothing while no keeper update is prepared")
        };
        if index % 3 == 2 {
            // Refused after admission, while earlier writers still hold the lane.
            ticket.release();
            continue;
        }
        let entered = Arc::clone(&entered);
        let inside = Arc::clone(&inside);
        tasks.push(tokio::spawn(async move {
            ticket.granted().await;
            {
                let mut held = inside.lock().unwrap();
                *held += 1;
                assert_eq!(*held, 1, "two keeper writes held the lane at once");
            }
            entered.lock().unwrap().push(index);
            tokio::task::yield_now().await;
            *inside.lock().unwrap() -= 1;
            ticket.release();
        }));
    }
    for task in tasks {
        task.await.expect("no admitted write panicked");
    }

    let granted = entered.lock().unwrap().clone();
    let expected: Vec<usize> = (0..WRITERS).filter(|index| index % 3 != 2).collect();
    assert_eq!(
        granted, expected,
        "the writers that did not refuse entered out of admit order"
    );
}
