//! Ports `apps/worker/tests/session/session-cell-scheduler.test.ts`: the leading
//! emit + 16 ms trailing coalesce, fenced to the stream generation it was armed
//! for, and the input-echo promotion. Driven through the production ingest and
//! `run_cadence_work` on a synthetic clock.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "cell_support/mod.rs"]
mod support;

use roost_worker::session::cell_gates::CellGate;
use roost_worker::session::cell_sink::COORD_CELL_SINK_ID;
use support::{Harness, row_text};

#[test]
fn identity_fences_a_cancelled_leading_emit_before_same_stream_rescheduling() {
    let mut harness = Harness::new(1);
    harness.enable_stream(1, 0);
    harness.write(b"\x1b[2;1HRESCHEDULED", 10);
    assert_eq!(
        harness.emitter.scheduled_emission(harness.channel),
        Some(false)
    );

    harness.emitter.cancel_cell_emission(harness.channel);
    assert_eq!(harness.emitter.scheduled_emission(harness.channel), None);
    let now = harness.at(10);
    harness
        .emitter
        .schedule_cell_emission(&mut harness.record, false, Harness::wall(10), now);
    harness.run(10);

    let attempts = harness.coord.attempts();
    assert_eq!(
        harness.coord.fulls(),
        vec![true, false],
        "exactly one leading delta"
    );
    assert_eq!(attempts[1].stream_id, support::stream_id(1));
    assert!(row_text(&attempts[1], 1).contains("RESCHEDULED"));
    assert_eq!(
        harness.emitter.scheduled_emission(harness.channel),
        Some(true),
        "the leading emit arms its trailing cooldown"
    );
}

#[test]
fn queued_old_work_touches_neither_a_replacement_stream_nor_a_suspended_sink() {
    let mut harness = Harness::new(2);
    harness.enable_stream(1, 0);
    harness.write(b"\x1b[2;1HREPLACED", 10);
    assert_eq!(
        harness.emitter.scheduled_emission(harness.channel),
        Some(false)
    );

    harness.enable_stream(2, 11);
    harness.run(12);
    let streams: Vec<String> = harness
        .coord
        .attempts()
        .iter()
        .map(|f| f.stream_id.clone())
        .collect();
    assert_eq!(streams, vec![support::stream_id(1), support::stream_id(2)]);

    harness.write(b"\x1b[3;1HCOORD-DOWN", 40);
    assert!(
        harness
            .emitter
            .scheduled_emission(harness.channel)
            .is_some()
    );
    harness.emitter.suspend_cell_sink(COORD_CELL_SINK_ID);
    harness.run(40);
    assert_eq!(harness.emitter.scheduled_emission(harness.channel), None);
    assert_eq!(
        harness.coord.attempts().len(),
        2,
        "no frame for a suspended sink"
    );

    // Resuming owes one full on the SAME generation, carrying the work that
    // landed while the coordinator was gone.
    harness.emitter.resume_cell_sink(COORD_CELL_SINK_ID);
    harness.run(41);
    let attempts = harness.coord.attempts();
    assert_eq!(attempts.len(), 3);
    assert!(attempts[2].full);
    assert_eq!(attempts[2].stream_id, support::stream_id(2));
    assert!(row_text(&attempts[2], 2).contains("COORD-DOWN"));
}

#[test]
fn a_gated_trailing_cooldown_waits_for_the_post_boundary_full() {
    let mut harness = Harness::new(3);
    harness.enable_stream(1, 0);
    harness.write(b"\x1b[2;1HLEADING", 10);
    harness.run(10);
    assert_eq!(harness.coord.attempts().len(), 2);

    harness.write(b"\x1b[3;1HGATED", 12);
    assert_eq!(
        harness.emitter.scheduled_emission(harness.channel),
        Some(true)
    );
    harness
        .emitter
        .hold_frames(harness.channel, CellGate::ResizeCapture, Harness::wall(13));
    harness.run(10 + 3 * 16);
    assert_eq!(harness.coord.attempts().len(), 2, "the gate leaked a frame");
    assert_eq!(harness.emitter.scheduled_emission(harness.channel), None);
    assert!(harness.emitter.is_dirty(harness.channel));

    harness.emitter.release_frames(harness.channel);
    let now = harness.at(80);
    harness
        .emitter
        .install_terminal_baseline_at(&mut harness.record, Harness::wall(80), now);
    let attempts = harness.coord.attempts();
    assert_eq!(attempts.len(), 3);
    assert!(attempts[2].full);
    assert!(row_text(&attempts[2], 2).contains("GATED"));
}

#[test]
fn schedules_normally_after_an_unclosed_synchronized_output_hold_trips() {
    let mut harness = Harness::new(4);
    harness.enable_stream(1, 0);
    harness.write(b"\x1b[?2026h\x1b[2;1HHELD", 10);
    harness.run(10);
    assert_eq!(harness.coord.attempts().len(), 1, "the hold leaked a frame");

    harness.run(10 + 1_000);
    assert_eq!(
        harness.emitter.sync_output_tripped(harness.channel),
        Some(true)
    );
    assert_eq!(
        harness.coord.attempts().len(),
        2,
        "the wall ceiling shipped the withheld frame"
    );

    harness.write(b"\x1b[3;1HAFTER-CAP", 1_020);
    harness.run(1_020);
    let attempts = harness.coord.attempts();
    assert_eq!(attempts.len(), 3);
    assert!(!attempts[2].full);
    assert!(row_text(&attempts[2], 2).contains("AFTER-CAP"));
}

#[test]
fn a_second_keystroke_in_one_coalesce_window_keeps_its_echo_promotion() {
    let mut harness = Harness::new(5);
    harness.enable_stream(1, 0);
    harness.emitter.note_input_echo(harness.channel);
    harness.emitter.note_input_echo(harness.channel);

    harness.write(b"\x1b[2;1HE1", 10);
    harness.run(10);
    let after_first_echo = harness.coord.attempts().len();
    assert_eq!(
        harness.emitter.scheduled_emission(harness.channel),
        Some(true)
    );

    harness.write(b"\x1b[3;1HE2", 12);
    harness.run(12);
    let attempts = harness.coord.attempts();
    assert_eq!(
        attempts.len(),
        after_first_echo + 1,
        "the promoted echo waited out the cooldown"
    );
    assert!(row_text(attempts.last().unwrap(), 2).contains("E2"));
    assert!(
        !harness
            .emitter
            .consume_input_echo_promotion(harness.channel)
    );
}

#[test]
fn a_trailing_cooldown_coalesces_a_burst_into_one_frame() {
    let mut harness = Harness::new(6);
    harness.enable_stream(1, 0);
    harness.write(b"\x1b[2;1Hone", 10);
    harness.run(10);
    harness.write(b"\x1b[3;1Htwo", 12);
    harness.write(b"\x1b[4;1Hthree", 14);
    harness.run(20);
    assert_eq!(
        harness.coord.attempts().len(),
        2,
        "a trailing frame fired inside the window"
    );
    harness.run(10 + 16);
    let attempts = harness.coord.attempts();
    assert_eq!(
        attempts.len(),
        3,
        "the burst coalesced into one trailing frame"
    );
    assert!(row_text(&attempts[2], 3).contains("three"));
    harness.run(10 + 32);
    assert_eq!(
        harness.coord.attempts().len(),
        3,
        "an idle cooldown emits nothing"
    );
    assert_eq!(harness.emitter.scheduled_emission(harness.channel), None);
}
