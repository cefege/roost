#![cfg(unix)]
//! PROOF that a keystroke's echo leaves the keeper as soon as the PTY produces
//! it, and reaches a worker waiting the way the worker's dispatch loop waits:
//! on the client's arrival bell, bounded by an idle timeout.
//!
//! WHY IT NEEDS A REAL DAEMON. The latency under test is the server loop's and
//! the client reader's, which no function a unit test can call has. A loop that
//! drains PTY output on a 16 ms tick, or a worker that polls on one, delivers
//! an echo a uniformly distributed 0–16 ms late — a median near 8 ms — and the
//! browser's predictive-echo overlay reads that lateness as a contradicted
//! prediction while the user is still typing.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use roost_keeper::client_connect::connect;
use roost_keeper::client_frames::EventPoll;
use roost_keeper::codec::MuxFrameType;
use roost_keeper::frames::ShellSpec;

mod support;

use support::daemon::{Keeper, TempDir};

/// Keystrokes measured. Enough for a median that one slow turn cannot move.
const KEYSTROKES: usize = 30;

/// The idle bound the worker's dispatch loop waits with.
const IDLE_WAIT: Duration = Duration::from_millis(16);

/// The median echo latency allowed. An event-driven path measures well under a
/// millisecond here; a 16 ms poll on either side of the socket measures ~8 ms.
const MEDIAN_ECHO: Duration = Duration::from_millis(4);

/// How long one echo may take before the test gives up on it.
const ECHO_DEADLINE: Duration = Duration::from_secs(2);

#[test]
fn a_keystroke_echo_reaches_a_bell_waiting_worker_without_a_tick() {
    let temp = TempDir::new("echo-latency");
    let keeper = Keeper::start(&temp);
    let client = connect(&keeper.endpoint()).expect("a real daemon completes the handshake");
    let cat = ShellSpec {
        program: "/bin/cat".to_owned(),
        args: Vec::new(),
        env: Vec::new(),
        cwd: None,
    };
    client.spawn(1, cat, 80, 24).expect("cat spawns");
    let arrival = client.arrival_bell();

    let mut latencies = Vec::with_capacity(KEYSTROKES);
    for index in 0..KEYSTROKES {
        let key = b'a' + (index % 26) as u8;
        // Out of step with any tick the loop might keep, so a tick-paced
        // drain shows its average lateness rather than a lucky phase.
        std::thread::sleep(Duration::from_millis(7));
        let sent = Instant::now();
        client
            .write_input(1, &[key])
            .expect("the keystroke is written");
        latencies.push(wait_for_echo(&client, &arrival, key, sent));
    }

    latencies.sort_unstable();
    let median = latencies[latencies.len() / 2];
    assert!(
        median < MEDIAN_ECHO,
        "the median keystroke echo took {median:?} (sorted: {latencies:?}); output is \
         waiting for a tick instead of being forwarded as it arrives"
    );
}

/// Wait as the worker's dispatch loop does — drain, then wait on the bell — until
/// a `PtyOut` carrying `key` arrives, and return how long that took.
fn wait_for_echo(
    client: &roost_keeper::client::KeeperClient,
    arrival: &roost_keeper::client_arrival::ArrivalBell,
    key: u8,
    sent: Instant,
) -> Duration {
    loop {
        assert!(
            sent.elapsed() < ECHO_DEADLINE,
            "the echo of {:?} never arrived",
            key as char
        );
        match client.poll_event() {
            EventPoll::Frame(frame) => {
                if frame.frame_type == MuxFrameType::PtyOut && frame.payload.contains(&key) {
                    return sent.elapsed();
                }
            }
            EventPoll::Empty => {
                arrival.wait(IDLE_WAIT);
            }
            EventPoll::Closed => panic!("the keeper closed the connection"),
        }
    }
}
