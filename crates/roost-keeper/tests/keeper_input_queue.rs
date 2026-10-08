#![cfg(unix)]
//! The keeper's per-channel input lane with a writer the test controls: one
//! FIFO across both frame kinds, the command and byte budgets, a batch whose
//! connection left before it started, and an exit that refuses what is still
//! queued. Pins v2 `apps/worker/src/keeper/keeper-input-queue.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::io::Write;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

use roost_keeper::input_queue::{
    InputLane, InputReply, InputRoute, KEEPER_INPUT_QUEUE_MAX_BYTES,
    KEEPER_INPUT_QUEUE_MAX_COMMANDS,
};
use roost_keeper::payloads::{PtyInRejectReason, PtyInResult};
use support::{DEADLINE, ResultTap, next_result};

/// A PTY write half that parks its first write until the test opens the gate,
/// and records every byte it accepted.
struct GatedWriter {
    gate: Option<Receiver<()>>,
    started: Sender<()>,
    accepted: Arc<Mutex<Vec<u8>>>,
}

impl Write for GatedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if let Some(gate) = self.gate.take() {
            self.started
                .send(())
                .expect("the test is waiting for the first write");
            gate.recv_timeout(DEADLINE)
                .expect("the test opens the gate");
        }
        self.accepted.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Wedged {
    lane: InputLane,
    open: Sender<()>,
    accepted: Arc<Mutex<Vec<u8>>>,
}

/// A lane whose first batch is stuck in the write until `open` is sent.
fn wedged_lane(first: &[u8]) -> Wedged {
    let (open, gate) = std::sync::mpsc::channel();
    let (started, write_started) = std::sync::mpsc::channel();
    let accepted = Arc::new(Mutex::new(Vec::new()));
    let writer = GatedWriter {
        gate: Some(gate),
        started,
        accepted: Arc::clone(&accepted),
    };
    let lane = InputLane::start(1, Box::new(writer)).expect("the lane thread starts");
    assert!(lane.enqueue(first.to_vec(), InputReply::Unacknowledged));
    write_started
        .recv_timeout(DEADLINE)
        .expect("the lane starts the first write");
    Wedged {
        lane,
        open,
        accepted,
    }
}

fn acknowledged(route: &Arc<InputRoute>, input_seq: u64) -> InputReply {
    InputReply::Acknowledged {
        input_seq,
        route: Arc::clone(route),
        generation: route.current(),
    }
}

fn tapped_route() -> (Arc<InputRoute>, Receiver<roost_keeper::codec::MuxFrame>) {
    let route = Arc::new(InputRoute::default());
    let (sender, results) = std::sync::mpsc::channel();
    route.attach(Arc::new(ResultTap(sender)));
    (route, results)
}

fn decoded(frame: &roost_keeper::codec::MuxFrame) -> PtyInResult {
    PtyInResult::decode(frame.frame_type, &frame.payload).expect("a result frame decodes")
}

/// The byte budget counts the batch being written: while it is stuck, the
/// queue refuses the first batch that would take the channel past 256 KiB.
#[test]
fn the_byte_budget_refuses_before_anything_is_queued() {
    let wedged = wedged_lane(&[b'a'; 1024]);
    let rest = KEEPER_INPUT_QUEUE_MAX_BYTES - 1024;
    assert!(
        wedged
            .lane
            .enqueue(vec![b'b'; rest], InputReply::Unacknowledged)
    );
    assert!(
        !wedged.lane.enqueue(vec![b'c'], InputReply::Unacknowledged),
        "one byte past the budget is refused"
    );
    wedged.open.send(()).unwrap();
    support::wait_until("the queue drains", || {
        wedged.accepted.lock().unwrap().len() == 1024 + rest
    });
    assert!(
        wedged.lane.enqueue(vec![b'd'], InputReply::Unacknowledged),
        "a drained lane admits again"
    );
}

/// The command budget likewise counts the in-flight batch.
#[test]
fn the_command_budget_refuses_the_two_hundred_and_first_batch() {
    let wedged = wedged_lane(b"0");
    for _ in 1..KEEPER_INPUT_QUEUE_MAX_COMMANDS {
        assert!(
            wedged
                .lane
                .enqueue(b"1".to_vec(), InputReply::Unacknowledged)
        );
    }
    assert!(
        !wedged
            .lane
            .enqueue(b"2".to_vec(), InputReply::Unacknowledged)
    );
    wedged.open.send(()).unwrap();
}

/// Legacy and acknowledged batches share one FIFO, so neither can overtake the
/// other; each acknowledged batch is answered with exactly what it wrote.
#[test]
fn both_frame_kinds_share_one_fifo_and_each_acknowledged_batch_is_answered() {
    let (route, results) = tapped_route();
    let wedged = wedged_lane(b"<");
    assert!(
        wedged
            .lane
            .enqueue(b"legacy-1 ".to_vec(), InputReply::Unacknowledged)
    );
    assert!(
        wedged
            .lane
            .enqueue(b"acked-2 ".to_vec(), acknowledged(&route, 2))
    );
    assert!(
        wedged
            .lane
            .enqueue(b"legacy-3 ".to_vec(), InputReply::Unacknowledged)
    );
    assert!(
        wedged
            .lane
            .enqueue(b"acked-4>".to_vec(), acknowledged(&route, 4))
    );
    wedged.open.send(()).unwrap();
    assert_eq!(
        decoded(&next_result(&results)),
        PtyInResult::Ack {
            input_seq: 2,
            written: 8
        }
    );
    assert_eq!(
        decoded(&next_result(&results)),
        PtyInResult::Ack {
            input_seq: 4,
            written: 8
        }
    );
    assert_eq!(
        wedged.accepted.lock().unwrap().as_slice(),
        b"<legacy-1 acked-2 legacy-3 acked-4>"
    );
}

/// v2 drops an unstarted acknowledged batch whose socket was destroyed: nobody
/// is left to be told, so it is never typed. The batch already writing
/// completes; its answer goes nowhere.
#[test]
fn an_unstarted_batch_whose_connection_left_is_never_written() {
    let (route, results) = tapped_route();
    let (started, write_started) = std::sync::mpsc::channel();
    let (open, gate) = std::sync::mpsc::channel();
    let accepted = Arc::new(Mutex::new(Vec::new()));
    let writer = GatedWriter {
        gate: Some(gate),
        started,
        accepted: Arc::clone(&accepted),
    };
    let lane = InputLane::start(1, Box::new(writer)).expect("the lane thread starts");
    assert!(lane.enqueue(b"started".to_vec(), acknowledged(&route, 1)));
    write_started.recv_timeout(DEADLINE).unwrap();
    assert!(lane.enqueue(b"orphaned".to_vec(), acknowledged(&route, 2)));
    route.detach();
    open.send(()).unwrap();
    support::wait_until("the started batch lands", || {
        accepted.lock().unwrap().len() >= 7
    });
    assert!(lane.enqueue(b"!".to_vec(), InputReply::Unacknowledged));
    support::wait_until("the lane drains", || {
        accepted.lock().unwrap().ends_with(b"!")
    });
    assert_eq!(accepted.lock().unwrap().as_slice(), b"started!");
    assert!(
        results.try_recv().is_err(),
        "a departed connection is told nothing"
    );
}

/// A child that exits refuses every batch still queued: nothing reached it, so
/// the answer is a rejection the client may retry elsewhere.
#[test]
fn an_exit_refuses_the_batches_still_queued() {
    let (route, results) = tapped_route();
    let wedged = wedged_lane(b"x");
    assert!(
        wedged
            .lane
            .enqueue(b"late".to_vec(), acknowledged(&route, 9))
    );
    wedged.lane.mark_exited();
    wedged.open.send(()).unwrap();
    assert_eq!(
        decoded(&next_result(&results)),
        PtyInResult::Reject {
            input_seq: 9,
            reason: PtyInRejectReason::ChildExited
        }
    );
    assert_eq!(wedged.accepted.lock().unwrap().as_slice(), b"x");
}

/// A PTY write half that accepts at most one pipe buffer per call, the way a
/// full pipe does, and counts the calls.
struct DribbleWriter {
    per_call: usize,
    accepted: Arc<Mutex<Vec<u8>>>,
    calls: Arc<Mutex<Vec<usize>>>,
}

impl Write for DribbleWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let taken = bytes.len().min(self.per_call);
        self.calls.lock().unwrap().push(taken);
        self.accepted
            .lock()
            .unwrap()
            .extend_from_slice(&bytes[..taken]);
        Ok(taken)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// One pipe buffer, which is what a PTY hands back when its input queue is full.
const PIPE_BUFFER: usize = 64 * 1024;

/// A batch larger than one pipe buffer is written WHOLE, in order, even though
/// the write half takes a buffer at a time. The legacy lane is owed no answer,
/// so a tail left behind here is a tail nobody is ever told about.
#[test]
fn a_batch_wider_than_one_pipe_buffer_reaches_the_pty_whole() {
    let accepted = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let writer = DribbleWriter {
        per_call: PIPE_BUFFER,
        accepted: Arc::clone(&accepted),
        calls: Arc::clone(&calls),
    };
    let lane = InputLane::start(1, Box::new(writer)).expect("the lane thread starts");
    let batch: Vec<u8> = (0..(PIPE_BUFFER * 2 + 7) as u32)
        .map(|index| b'a' + (index % 26) as u8)
        .collect();
    assert!(lane.enqueue(batch.clone(), InputReply::Unacknowledged));
    support::wait_until("the whole batch lands", || {
        accepted.lock().unwrap().len() == batch.len()
    });
    assert_eq!(
        *accepted.lock().unwrap(),
        batch,
        "every byte of a multi-buffer batch reaches the PTY, in order"
    );
    assert_eq!(
        calls.lock().unwrap().as_slice(),
        [PIPE_BUFFER, PIPE_BUFFER, 7],
        "the batch is drained across three writes, largest chunk first"
    );
}

/// The same batch on the acknowledged lane is ANSWERED as complete. A short
/// write reported as ambiguous would be right only if bytes were actually lost;
/// on a draining pipe they are not, and a client that must not retry an
/// ambiguous batch would otherwise lose the tail of every large paste.
#[test]
fn a_batch_wider_than_one_pipe_buffer_is_acknowledged_complete() {
    let (route, results) = tapped_route();
    let accepted = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let writer = DribbleWriter {
        per_call: 4096,
        accepted: Arc::clone(&accepted),
        calls: Arc::clone(&calls),
    };
    let lane = InputLane::start(1, Box::new(writer)).expect("the lane thread starts");
    let batch = vec![b'z'; PIPE_BUFFER + 3];
    assert!(lane.enqueue(batch.clone(), acknowledged(&route, 5)));
    assert_eq!(
        decoded(&next_result(&results)),
        PtyInResult::Ack {
            input_seq: 5,
            written: batch.len() as u32
        }
    );
    assert_eq!(accepted.lock().unwrap().len(), batch.len());
    assert!(calls.lock().unwrap().len() > 1);
}

/// A write half that refuses outright is still reported, and still distinguishes
/// "nothing landed" from "some landed", because only the second one licenses a
/// duplicated keystroke if a client were to retry.
#[test]
fn a_write_half_that_refuses_is_still_reported() {
    struct RefusingWriter;
    impl Write for RefusingWriter {
        fn write(&mut self, _bytes: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("no reader"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let (route, results) = tapped_route();
    let lane = InputLane::start(1, Box::new(RefusingWriter)).expect("the lane thread starts");
    assert!(lane.enqueue(b"gone".to_vec(), acknowledged(&route, 6)));
    assert_eq!(
        decoded(&next_result(&results)),
        PtyInResult::Reject {
            input_seq: 6,
            reason: PtyInRejectReason::NoReader
        }
    );
}
