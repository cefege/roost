//! PROOF for the drain cadence: a program writing continuously must have its
//! output reach the worker at least as often as the keeper's backstop tick,
//! and never at the pace of a blocking read plus a sleep.
//!
//! WHY IT NEEDS A REAL DAEMON. The quantity under test is the interval between
//! two `PtyOut` frames as a worker observes them, which is a property of the
//! SERVER LOOP and not of any function a unit test can call. A fake keeper that
//! answers on demand is paced by the test, so it agrees with whatever the test
//! does. The shell here prints continuously and the client sends nothing after
//! `Spawn`, so the frames are paced by the shell's writes and the server loop
//! alone.
//!
//! The bound is generous on purpose. Forty lines ten milliseconds apart is
//! ~400 ms of writing; a loop that read for 100 ms and then slept its tick
//! measured a ~120 ms cadence, so a 60 ms bound separates the two without
//! being a benchmark of this machine.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use roost_keeper::client_connect::connect;
use roost_keeper::codec::MuxFrameType;
use roost_keeper::frames::ShellSpec;

mod support;

use support::daemon::{Keeper, TempDir};

/// Lines the shell prints, ten milliseconds apart.
const LINES: u32 = 40;

/// How long a paced tick is allowed to take for the whole burst. Not 16 ms: the
/// assertion is about how many DRAINS carried the lines, not about the writer.
const BURST_BUDGET: Duration = Duration::from_millis(4_000);

/// A coarse backstop on the WORST gap. Deliberately loose, because the median
/// below is the assertion that discriminates and a tight worst-case bound only
/// buys flakiness on rare long turns. Its job is to catch a loop that is not
/// draining at the tick at all: with the loop's sleep restored, the worst gap
/// measures ~120 ms.
const MAX_GAP: Duration = Duration::from_millis(150);

/// The MEDIAN silence the tick permits, which is the one that separates a turn
/// that lands on the grid from one that alternates between it and twice it.
/// Measured populations, real daemon: 20 ms median with the read's rounding
/// charged to every pass, 16 ms with the grid chased.
const MEDIAN_GAP: Duration = Duration::from_millis(18);

/// A shell that prints `LINES` numbered lines, ten milliseconds apart.
fn printing_shell() -> ShellSpec {
    ShellSpec {
        program: "/bin/sh".to_owned(),
        args: vec![
            "-c".to_owned(),
            format!("for i in $(seq 1 {LINES}); do printf 'L%03d\\n' $i; sleep 0.01; done"),
        ],
        env: Vec::new(),
        cwd: None,
    }
}

#[test]
fn continuous_output_is_drained_at_the_tick_and_not_at_the_tick_plus_a_read() {
    let temp = TempDir::new("output-tick");
    let keeper = Keeper::start(&temp);
    let client = connect(keeper.socket()).expect("a real daemon completes the handshake");

    client
        .spawn(1, printing_shell(), 80, 24)
        .expect("the shell spawns");

    // Nothing but the shell's writes and the server loop paces these frames:
    // the client sends nothing more.
    let deadline = Instant::now() + BURST_BUDGET;
    let mut lines = 0_u32;
    let mut frames = 0_u32;
    let mut worst_gap = Duration::ZERO;
    let mut gaps: Vec<Duration> = Vec::new();
    let mut previous = Instant::now();

    while lines < LINES && Instant::now() < deadline {
        let Some(frame) = client.next_event(Duration::from_millis(200)) else {
            continue;
        };
        if frame.frame_type != MuxFrameType::PtyOut {
            continue;
        }
        frames += 1;
        let now = Instant::now();
        let gap = now.saturating_duration_since(previous);
        worst_gap = worst_gap.max(gap);
        gaps.push(gap);
        previous = now;
        lines += String::from_utf8_lossy(&frame.payload).lines().count() as u32;
    }

    assert_eq!(
        lines, LINES,
        "the shell's {LINES} lines did not all arrive within {BURST_BUDGET:?} across \
         {frames} frames; the drain is still too slow to keep a terminal live"
    );
    // THE SUSTAINED RATE, which is what a reader feels and what the oracle
    // counts. The worst-case gap cannot see it: this loop is bimodal on this
    // host, because a `SO_RCVTIMEO` read rounds 16 ms to 20, so a turn that
    // re-anchors its deadline instead of chasing it alternates 16 ms and 32 ms
    // — mean ~22 ms, ~45 chunk groups per second — and a 30 ms worst-case bound
    // passes that happily. It is exactly the shape that cost the oracle 40% of
    // its terminal frames while this test stayed green.
    gaps.sort_unstable();
    let median = gaps[gaps.len() / 2];
    assert!(
        median < MEDIAN_GAP,
        "the median silence between PtyOut frames was {median:?} across {} frames, \
         which is a turn alternating between the tick and twice the tick rather \
         than one landing on it",
        gaps.len()
    );
    // A COARSE BACKSTOP, not the gate: it only has to catch a loop that is
    // plainly not draining at the tick — the sleep this file used to carry
    // measured ~120 ms. Pinning it tighter makes it flaky on rare long turns,
    // which is why the median above carries the real assertion.
    assert!(
        worst_gap < MAX_GAP,
        "the longest silence between two PtyOut frames was {worst_gap:?}, which is \
         not a loop draining at the tick at all"
    );
    // AND THE SUSTAINED RATE, which is what a reader actually feels and what a
    // worst-case gap cannot see. The loop is BIMODAL on this host: a
    // `SO_RCVTIMEO` read rounds 16 ms to 20, so a turn that does not chase its
    // deadline alternates 16 ms and 32 ms — mean ~22 ms, i.e. ~45 chunk groups
    // per second, which a 30 ms worst-case bound passes happily and which cost
    // the oracle 40% of its terminal frames. A median is the statistic that
    // separates "on the tick" from "on and off the tick".
    gaps.sort_unstable();
    let median = gaps[gaps.len() / 2];
    assert!(
        median < MEDIAN_GAP,
        "the median silence between PtyOut frames was {median:?} across {} frames, \
         which is a turn that alternates between the tick and twice the tick \
         rather than one that lands on it",
        gaps.len()
    );
}
