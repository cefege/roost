#![cfg(unix)]
//! PROOF for the drain cadence: output a program writes while the worker sends
//! nothing must reach the worker within the keeper's backstop tick of being
//! written, and never at the pace of a blocking read plus a sleep.
//!
//! WHY IT NEEDS A REAL DAEMON. The quantity under test is how long the SERVER
//! LOOP holds output a channel's reader already has, which is not a property of
//! any function a unit test can call. The client sends nothing after `Spawn`,
//! so only PTY output and the tick can wake that loop.
//!
//! WHY LATENCY AND NOT THE GAP BETWEEN FRAMES. A frame gap is the slower of the
//! loop's pace and the writer's, so a writer slower than the tick makes every
//! gap its own interval whatever the loop does. A shell paced by `sleep 0.01`
//! pays a fork and an exec per line, which a macOS CI runner stretches to
//! ~60 ms: forty frames of one line each and a 59 ms median gap, from a loop
//! that never once held a line back. So the test releases each line itself,
//! through a FIFO the keeper never sees, and times it from release to arrival;
//! the shell between the two runs only builtins.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

use roost_keeper::client_connect::connect;
use roost_keeper::codec::MuxFrameType;
use roost_keeper::frames::ShellSpec;

mod support;

use support::daemon::{DEADLINE, Keeper, TempDir};

/// Lines the test releases to the shell.
const LINES: usize = 40;

/// How often a line is released. Faster than the tick and out of step with it,
/// so the loop always has output in flight and a tick-paced drain shows its
/// average lateness rather than a lucky phase.
const RELEASE_INTERVAL: Duration = Duration::from_millis(10);

/// The percentile of release-to-arrival latency the bound below applies to.
/// The fifth left over absorbs a loaded runner's scheduling rather than a
/// drain's lateness.
const ON_TICK_PERCENTILE: usize = 80;

/// The most the percentile line may take: the keeper's 16 ms `OUTPUT_TICK`
/// plus a quarter tick for the turn's own work. Measured populations of that
/// line, real daemon: ~1 ms for a loop forwarding on its reader's wake (4–10 ms
/// with the whole stack pinned to one core behind four busy loops), 14–16 ms
/// for a turn landing on the tick, 23–25 ms for one alternating between the
/// tick and twice the tick, ~96 ms for a 100 ms read followed by the tick.
const ON_TICK_BOUND: Duration = Duration::from_millis(20);

/// A coarse backstop on the WORST line, for a stall the percentile cannot see:
/// one line held far past any tick while the rest flow. Deliberately loose,
/// because a loaded runner's rare long turn is not a defect.
const MAX_LATENCY: Duration = Duration::from_millis(150);

/// How long the receive wait blocks once every line is released.
const ARRIVAL_POLL: Duration = Duration::from_millis(200);

/// A shell that writes back every line it reads from `fifo`. `read` and
/// `printf` are builtins of every `/bin/sh`, so nothing between a release and
/// its write to the PTY forks.
fn echoing_shell(fifo: &Path) -> ShellSpec {
    ShellSpec {
        program: "/bin/sh".to_owned(),
        args: vec![
            "-c".to_owned(),
            format!(
                "while IFS= read -r line; do printf '%s\\n' \"$line\"; done < '{}'",
                fifo.display()
            ),
        ],
        env: Vec::new(),
        cwd: None,
    }
}

#[test]
fn continuous_output_is_drained_at_the_tick_and_not_at_the_tick_plus_a_read() {
    let temp = TempDir::new("output-tick");
    let fifo = temp.path().join("lines.fifo");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("mkfifo runs");
    assert!(made.success(), "mkfifo {} failed: {made}", fifo.display());

    let keeper = Keeper::start(&temp);
    let client = connect(&keeper.endpoint()).expect("a real daemon completes the handshake");
    client
        .spawn(1, echoing_shell(&fifo), 80, 24)
        .expect("the shell spawns");
    let mut release = open_release_end(&fifo);

    let deadline = Instant::now() + DEADLINE;
    let mut released: Vec<Instant> = Vec::with_capacity(LINES);
    let mut arrived: Vec<Option<Instant>> = vec![None; LINES];
    let mut pending: Vec<u8> = Vec::new();
    let mut next_release = Instant::now();

    while arrived.iter().any(Option::is_none) && Instant::now() < deadline {
        if released.len() < LINES && Instant::now() >= next_release {
            let line = format!("L{:03}\n", released.len());
            let at = Instant::now();
            release
                .write_all(line.as_bytes())
                .expect("the shell holds the FIFO's read end");
            released.push(at);
            next_release = at + RELEASE_INTERVAL;
        }
        let wait = if released.len() < LINES {
            next_release.saturating_duration_since(Instant::now())
        } else {
            ARRIVAL_POLL
        };
        let Some(frame) = client.next_event(wait) else {
            continue;
        };
        let at = Instant::now();
        if frame.frame_type != MuxFrameType::PtyOut || frame.channel_id != 1 {
            continue;
        }
        pending.extend_from_slice(&frame.payload);
        for index in take_line_indices(&mut pending) {
            if let Some(slot) = arrived.get_mut(index) {
                slot.get_or_insert(at);
            }
        }
    }

    let missing = arrived.iter().filter(|slot| slot.is_none()).count();
    assert_eq!(
        missing, 0,
        "{missing} of the {LINES} released lines never reached the worker within {DEADLINE:?}"
    );
    let mut latencies: Vec<Duration> = released
        .iter()
        .zip(&arrived)
        .map(|(sent, landed)| {
            landed
                .expect("every line arrived")
                .saturating_duration_since(*sent)
        })
        .collect();
    latencies.sort_unstable();

    let on_tick = latencies[latencies.len() * ON_TICK_PERCENTILE / 100];
    assert!(
        on_tick < ON_TICK_BOUND,
        "the {ON_TICK_PERCENTILE}th-percentile line took {on_tick:?} from its write to the \
         worker, past {ON_TICK_BOUND:?}: the loop holds output for a turn that does not land \
         on the tick (sorted: {latencies:?})"
    );
    let worst = latencies[latencies.len() - 1];
    assert!(
        worst < MAX_LATENCY,
        "the slowest line took {worst:?} from its write to the worker, held far past any \
         tick (sorted: {latencies:?})"
    );
}

/// Open the FIFO's write end once the shell holds its read end. Non-blocking,
/// because a blocking open waits for a reader forever, and a shell that never
/// started must fail this test rather than hang it.
fn open_release_end(fifo: &Path) -> File {
    let deadline = Instant::now() + DEADLINE;
    loop {
        match OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(fifo)
        {
            Ok(file) => return file,
            // ENXIO is a FIFO with no reader yet: the shell has not reached
            // its redirection.
            Err(err) if err.raw_os_error() == Some(libc::ENXIO) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(err) => panic!("the shell never opened {}: {err}", fifo.display()),
        }
    }
}

/// Take every complete line out of `pending` and return the release index each
/// one names; a partial line stays for the next frame.
fn take_line_indices(pending: &mut Vec<u8>) -> Vec<usize> {
    let mut indices = Vec::new();
    while let Some(end) = pending.iter().position(|&byte| byte == b'\n') {
        let line: Vec<u8> = pending.drain(..=end).collect();
        let text = String::from_utf8_lossy(&line);
        if let Some(index) = text
            .trim_end()
            .strip_prefix('L')
            .and_then(|digits| digits.parse().ok())
        {
            indices.push(index);
        }
    }
    indices
}
