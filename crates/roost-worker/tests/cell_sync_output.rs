//! Ports `apps/worker/tests/terminal/terminal-stream-sync-output.test.ts`: the
//! DEC 2026 hold withholds frames inside a synchronized frame, the 1 s wall and
//! 2 000-row ceilings each release it exactly once, and a normal close flushes
//! once. Plus the byte scanner that tracks the mode beside the core.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "cell_support/mod.rs"]
mod support;

use roost_worker::session::sync_output::SyncOutputScan;
use roost_worker::stream_fence::SYNC_OUTPUT_MAX_PENDING_ROWS;
use support::{Harness, TEST_ROWS, row_text};

const WALL_CAP_MS: u64 = 1_000;

#[test]
fn the_wall_cap_installs_an_initial_full_while_the_generation_stays_open() {
    let mut harness = Harness::new(21);
    harness.write(b"\x1b[?2026h\x1b[1;1HHELD", 0);
    assert!(harness.emitter.synchronized_output(harness.channel));
    harness.enable_stream(1, 5);
    assert!(
        harness.coord.attempts().is_empty(),
        "the baseline leaked out of the synchronized frame"
    );
    assert_eq!(
        harness.emitter.sync_output_tripped(harness.channel),
        Some(false)
    );

    harness.run(5 + WALL_CAP_MS - 1);
    assert!(
        harness.coord.attempts().is_empty(),
        "the wall ceiling fired early"
    );

    harness.run(5 + WALL_CAP_MS);
    assert!(harness.emitter.synchronized_output(harness.channel));
    let attempts = harness.coord.attempts();
    assert_eq!(attempts.len(), 1);
    assert!(attempts[0].full, "the owed baseline shipped as a delta");
    assert_eq!((attempts[0].seq, attempts[0].base_seq), (1, 0));
    assert!(row_text(&attempts[0], 0).contains("HELD"));
    assert_eq!(
        harness.emitter.sync_output_tripped(harness.channel),
        Some(true)
    );

    harness.write(b"\x1b[2;1HAFTER-CAP", 5 + WALL_CAP_MS + 10);
    harness.emit(false, 5 + WALL_CAP_MS + 10);
    let attempts = harness.coord.attempts();
    assert_eq!(attempts.len(), 2);
    assert!(!attempts[1].full);
    assert_eq!((attempts[1].seq, attempts[1].base_seq), (2, 1));
    assert!(row_text(&attempts[1], 1).contains("AFTER-CAP"));
    assert_eq!(
        harness.emitter.sync_output_tripped(harness.channel),
        Some(true),
        "the tripped hold was replaced"
    );
}

#[test]
fn the_pending_row_cap_releases_an_owed_full_and_leaves_the_generation_pass_through() {
    let mut harness = Harness::new(22);
    harness.enable_stream(1, 0);
    assert_eq!(harness.coord.attempts().len(), 1);

    harness.write(b"\x1b[?2026h", 10);
    harness.emit(true, 10);
    assert_eq!(
        harness.coord.attempts().len(),
        1,
        "a forced full leaked out of the synchronized frame"
    );
    assert_eq!(
        harness.emitter.sync_output_tripped(harness.channel),
        Some(false)
    );

    let flood = "X\r\n".repeat(SYNC_OUTPUT_MAX_PENDING_ROWS as usize + TEST_ROWS as usize);
    harness.write(flood.as_bytes(), 20);
    assert!(harness.emitter.synchronized_output(harness.channel));
    let attempts = harness.coord.attempts();
    assert_eq!(
        attempts.len(),
        2,
        "the row ceiling did not release the withheld frame"
    );
    assert!(attempts[1].full);
    assert_eq!((attempts[1].seq, attempts[1].base_seq), (2, 0));
    assert_eq!(
        harness.emitter.sync_output_tripped(harness.channel),
        Some(true)
    );

    harness.run(10 + WALL_CAP_MS + 5);
    assert_eq!(
        harness.coord.attempts().len(),
        2,
        "a tripped hold's wall ceiling fired anyway"
    );

    harness.write(b"\x1b[1;1HPASS", 1_100);
    harness.run(1_100);
    let attempts = harness.coord.attempts();
    assert_eq!(attempts.len(), 3);
    assert!(!attempts[2].full);
    assert_eq!((attempts[2].seq, attempts[2].base_seq), (3, 2));
}

#[test]
fn a_normal_close_before_the_wall_cap_flushes_exactly_once() {
    let mut harness = Harness::new(23);
    harness.enable_stream(1, 0);
    assert_eq!(harness.coord.attempts().len(), 1);

    harness.write(b"\x1b[?2026h\x1b[3;1HCLOSE-FLUSH", 10);
    harness.run(10);
    assert_eq!(
        harness.coord.attempts().len(),
        1,
        "a frame leaked out of the synchronized frame"
    );

    harness.run(10 + WALL_CAP_MS - 1);
    assert_eq!(harness.coord.attempts().len(), 1);

    harness.write(b"\x1b[?2026l", 10 + WALL_CAP_MS - 1);
    assert!(!harness.emitter.synchronized_output(harness.channel));
    let attempts = harness.coord.attempts();
    assert_eq!(
        attempts.len(),
        2,
        "the close did not flush the withheld frame"
    );
    assert!(!attempts[1].full);
    assert_eq!((attempts[1].seq, attempts[1].base_seq), (2, 1));
    assert!(row_text(&attempts[1], 2).contains("CLOSE-FLUSH"));
    assert!(!harness.emitter.sync_output_held(harness.channel));

    harness.run(10 + 2 * WALL_CAP_MS);
    assert_eq!(
        harness.coord.attempts().len(),
        2,
        "a released hold fired its ceiling"
    );
}

#[test]
fn the_scanner_follows_mode_2026_across_chunks_and_parameter_lists() {
    let mut scan = SyncOutputScan::default();
    scan.observe(b"text\x1b[?20");
    assert!(!scan.open, "a split opener counted before it completed");
    scan.observe(b"26h more");
    assert!(scan.open);
    assert_eq!(scan.generation, 1);

    scan.observe(b"\x1b[?2026h");
    assert_eq!(
        scan.generation, 1,
        "a re-open inside an open frame is not a new generation"
    );
    scan.observe(b"\x1b[?25;2026l");
    assert!(!scan.open, "2026 inside a parameter list was missed");

    scan.observe(b"\x1b[?1049;2026h\x1b[?2026l\x1b[?2026h");
    assert!(scan.open);
    assert_eq!(
        scan.generation, 3,
        "each closed-to-open transition is a generation"
    );

    scan.observe(b"\x1b[?20261l\x1b[2026l\x1b]2;?2026l\x07");
    assert!(
        scan.open,
        "a different mode, an ANSI mode, or an OSC body closed the frame"
    );
}
