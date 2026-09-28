//! The startup meter's observable contract, ported from
//! `apps/web/tests/renderer/terminalStartupProgress.test.ts` against
//! `startup_progress.rs`: contiguous ordered bands, a creep that stays inside
//! its own band, chunk subdivision that can overtake but never escape, a floor
//! that absorbs status regressions, and the 99 ceiling that keeps 100 for
//! completion.

use roost_web_terminal::startup_progress::{
    StartupChunks, TerminalStartupSample, TerminalStartupStage, terminal_startup_chunk_detail,
    terminal_startup_percent,
};

const ORDERED_STAGES: [TerminalStartupStage; 5] = [
    TerminalStartupStage::Spawn,
    TerminalStartupStage::Measure,
    TerminalStartupStage::Viewport,
    TerminalStartupStage::Frame,
    TerminalStartupStage::Render,
];

fn percent(
    stage: TerminalStartupStage,
    stage_elapsed_ms: f64,
    chunks: Option<(u32, u32)>,
    floor: f64,
) -> f64 {
    terminal_startup_percent(TerminalStartupSample {
        stage,
        stage_elapsed_ms,
        chunks: chunks.map(|(received, total)| StartupChunks { received, total }),
        floor,
    })
}

#[test]
fn the_five_forward_pane_stages_tile_46_to_99_with_no_gap_or_overlap() {
    let mut cursor = TerminalStartupStage::Spawn.step().start;
    assert_eq!(cursor, 46.0);
    for stage in ORDERED_STAGES {
        let step = stage.step();
        assert_eq!(step.start, cursor, "{stage:?} starts where the last ended");
        assert!(step.end > step.start, "{stage:?} owns a band");
        cursor = step.end;
    }
    assert_eq!(cursor, 99.0);
}

#[test]
fn retry_is_zero_width_so_a_reconnecting_step_cannot_advance() {
    let retry = TerminalStartupStage::Retry.step();
    assert_eq!(retry.start, retry.end);
    assert_eq!(
        percent(TerminalStartupStage::Retry, 5_000.0, None, 91.0),
        91.0
    );
}

#[test]
fn a_waiting_step_creeps_forward_but_never_leaves_its_own_band() {
    assert_eq!(percent(TerminalStartupStage::Spawn, 0.0, None, 0.0), 46.0);
    let one_time_constant = percent(TerminalStartupStage::Spawn, 900.0, None, 0.0);
    assert!(one_time_constant > 55.0, "{one_time_constant}");
    assert!(one_time_constant < 62.0, "{one_time_constant}");
    assert_eq!(
        one_time_constant, 56.1,
        "about 63% of the 16-point band after one time constant, to a tenth"
    );
    assert!(percent(TerminalStartupStage::Spawn, 60_000.0, None, 0.0) < 62.0);
}

#[test]
fn chunked_assembly_subdivides_the_frame_band_and_outruns_the_creep() {
    assert_eq!(
        percent(TerminalStartupStage::Frame, 0.0, Some((7, 7)), 0.0),
        96.0
    );
    assert_eq!(
        percent(TerminalStartupStage::Frame, 0.0, Some((0, 7)), 0.0),
        82.0
    );
}

#[test]
fn a_racing_or_unusable_chunk_count_cannot_leave_the_band() {
    assert_eq!(
        percent(TerminalStartupStage::Frame, 0.0, Some((9, 7)), 0.0),
        96.0,
        "a count past its total is the total"
    );
    // An unusable total falls back to pure time creep, which at 0ms is the
    // floor of the band rather than a division by zero.
    assert_eq!(
        percent(TerminalStartupStage::Frame, 0.0, Some((1, 0)), 0.0),
        82.0
    );
}

#[test]
fn the_floor_absorbs_a_stage_regression_instead_of_rewinding_the_bar() {
    assert_eq!(
        percent(TerminalStartupStage::Measure, 0.0, None, 88.0),
        88.0
    );
}

#[test]
fn nothing_reaches_100_only_completion_does() {
    assert_eq!(
        percent(TerminalStartupStage::Render, 600_000.0, None, 99.0),
        99.0
    );
    assert_eq!(
        percent(TerminalStartupStage::Render, 600_000.0, Some((7, 7)), 100.0),
        99.0,
        "not even a floor that already shows completion"
    );
}

#[test]
fn a_chunk_count_reads_as_a_human_part_count() {
    assert_eq!(
        terminal_startup_chunk_detail(Some(StartupChunks {
            received: 3,
            total: 7
        })),
        Some("part 3 of 7".to_string())
    );
    assert_eq!(
        terminal_startup_chunk_detail(Some(StartupChunks {
            received: 9,
            total: 7
        })),
        Some("part 7 of 7".to_string()),
        "a racing count is clamped to its total"
    );
}

#[test]
fn unusable_counts_produce_no_line_at_all() {
    assert_eq!(terminal_startup_chunk_detail(None), None);
    assert_eq!(
        terminal_startup_chunk_detail(Some(StartupChunks {
            received: 0,
            total: 0
        })),
        None
    );
}
