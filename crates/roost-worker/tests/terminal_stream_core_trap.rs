//! The core-trap half of the stream contract: an unprovable keeper boundary
//! latches the core fail-closed and hands back both its capture and its
//! emission gate, no later generation escapes the latch without a re-proof,
//! and the next desire re-proves the core from the keeper's ordered history.
//! Ports `apps/worker/tests/terminal/terminal-stream-core-trap.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod terminal_stream_support;

use roost_keeper::history::HistoryRecord;
use roost_protocol::wire::coord_worker::TerminalStreamFailureKind as Failure;
use roost_term::RioCore;
use roost_worker::session::keeper_channels::SurvivorHistory;
use roost_worker::session::resize::ResizeOutcome;
use roost_worker::session::terminal_state::WorkerStreamResult;
use terminal_stream_support::{
    Answer, COLS, Harness, ROWS, STREAM_A, STREAM_B, STREAM_C, channel, held, session_id,
};

const REPROVED_COLS: u16 = COLS + 6;
const REPROVED_ROWS: u16 = ROWS + 2;

/// An acknowledgement for geometry the worker never asked for: the boundary
/// can never be applied, so the core traps (v2 `trapResizeCapture`).
async fn trap(harness: &Harness) -> WorkerStreamResult {
    held(&harness.keeper.script).push_back(Answer::AckAt {
        cols: COLS + 9,
        rows: ROWS,
    });
    harness.enable(STREAM_A, COLS + 4, ROWS).await
}

/// The emission gate ITSELF and the row that attributes it. The gate is what
/// withholds a channel's frames, and a stranded one outlives every event that
/// could lift it; a stranded row keeps blaming a capture that is already gone.
fn gate_released(harness: &Harness) -> bool {
    let emitter = held(&harness.emitter);
    !emitter.gate_held(channel()) && emitter.gate_suppression(channel()).is_none()
}

#[tokio::test]
async fn a_trapped_resize_reports_a_reprovable_core_and_releases_its_gate() {
    let harness = Harness::scripted(RioCore::new(COLS, ROWS));
    let result = trap(&harness).await;
    assert!(
        matches!(&result, WorkerStreamResult::Ambiguous { failure: Failure::CoreFailed, reason, .. }
        if reason == "keeper acknowledged conflicting resize geometry"),
        "{result:?}"
    );
    assert!(!harness.core_valid());
    assert!(
        gate_released(&harness),
        "a capture nothing can finish must not keep the gate"
    );
    assert_eq!(
        harness.manager.current_terminal_stream_id(&session_id()),
        None
    );
    assert_eq!(
        harness.with_record(|record| (record.terminal_core.cols(), record.terminal_core.rows())),
        (COLS, ROWS)
    );

    // The capture itself is gone, not just its gate: one that outlived the
    // failure would refuse every later boundary on this channel, so the second
    // resize is admitted and reaches the keeper.
    let later = harness
        .manager
        .resize_channel(channel(), COLS + 4, ROWS)
        .expect("the session is held");
    assert!(
        matches!(later, ResizeOutcome::Applied { .. }),
        "a stranded capture refused a later boundary: {later:?}"
    );
    assert_eq!(
        held(&harness.keeper.resized).len(),
        2,
        "the trap's boundary and the one after it are the only writes"
    );
}

#[tokio::test]
async fn a_trapped_core_refuses_frames_and_parses_nothing_for_its_generation() {
    let harness = Harness::scripted(RioCore::new(COLS, ROWS));
    harness.enable(STREAM_A, COLS, ROWS).await;
    held(&harness.keeper.script).push_back(Answer::AckAt {
        cols: COLS + 9,
        rows: ROWS,
    });
    harness.enable(STREAM_B, COLS + 4, ROWS).await;
    let frames = held(&harness.sink.frames).len();
    let head = harness.with_record(|record| record.head_seq);
    harness.deliver(b"\x1b[3;1HAFTER-TRAP");
    harness
        .manager
        .request_terminal_snapshot(&session_id(), STREAM_B);
    assert_eq!(
        held(&harness.sink.frames).len(),
        frames,
        "a trapped core builds no frame"
    );
    assert_eq!(
        harness.with_record(|record| record.head_seq),
        head + 16,
        "the retain lane still keeps every byte"
    );
    let row = harness.with_record(|record| {
        (0..COLS)
            .filter_map(|col| char::from_u32(record.terminal_core.viewport_cell(2, col).character))
            .collect::<String>()
    });
    assert!(
        !row.contains("AFTER"),
        "the frozen core never parsed the retained bytes: {row:?}"
    );
}

#[tokio::test]
async fn a_generation_minted_after_a_trap_that_cannot_be_reproved_stays_closed() {
    let harness = Harness::scripted(RioCore::new(COLS, ROWS));
    trap(&harness).await;
    let frames = held(&harness.sink.frames).len();
    let refused = harness.enable(STREAM_B, REPROVED_COLS, REPROVED_ROWS).await;
    assert!(
        matches!(&refused, WorkerStreamResult::Rejected { failure: Failure::CoreFailed, reason, .. }
        if reason.starts_with("terminal core re-proof failed: keeper history unavailable")),
        "{refused:?}"
    );
    assert!(!harness.core_valid());
    assert!(gate_released(&harness));
    assert_eq!(held(&harness.sink.frames).len(), frames);
    assert_eq!(
        held(&harness.keeper.resized).len(),
        1,
        "a core that cannot be re-proved is never resized"
    );
    // Once something re-proves the core, no stranded gate keeps the new generation silent.
    held(&harness.delivery)
        .stream_emission()
        .unwrap()
        .prove_core(channel());
    harness
        .manager
        .request_terminal_snapshot(&session_id(), STREAM_B);
    let last = harness
        .fulls()
        .pop()
        .expect("a baseline after the re-proof");
    assert_eq!(last.stream_id, STREAM_B);
    assert_eq!(
        held(&harness.sink.frames).len(),
        frames + 1,
        "the re-proof owes this generation one full and nothing was held back"
    );
}

#[tokio::test]
async fn a_fail_closed_core_is_reproved_from_keeper_history_on_the_next_desire() {
    let harness = Harness::scripted(RioCore::new(COLS, ROWS));
    harness.enable(STREAM_A, COLS, ROWS).await;
    harness.deliver(b"BEFORE-TRAP");
    held(&harness.keeper.script).push_back(Answer::AckAt {
        cols: COLS + 9,
        rows: ROWS,
    });
    harness.enable(STREAM_C, COLS + 4, ROWS).await;
    assert!(!harness.core_valid());
    harness.deliver(b"\r\nAFTER-TRAP");
    let frozen_head = harness.with_record(|record| record.head_seq);
    let frozen_epoch = harness.with_record(|record| record.cell_emit.grid_epoch_base.clone());
    *held(&harness.keeper.history) = Some(SurvivorHistory {
        records: vec![
            HistoryRecord::Output {
                bytes: b"BEFORE-TRAP".to_vec(),
            },
            HistoryRecord::Output {
                bytes: b"\r\nAFTER-TRAP".to_vec(),
            },
        ],
        head_seq: frozen_head,
        base_cols: COLS,
        base_rows: ROWS,
    });
    let result = harness.enable(STREAM_B, REPROVED_COLS, REPROVED_ROWS).await;
    assert!(
        matches!(
            result,
            WorkerStreamResult::Committed {
                resized: true,
                channel_resize_seq: 2,
                ..
            }
        ),
        "{result:?}"
    );
    assert!(harness.core_valid());
    assert_ne!(
        harness.with_record(|record| record.cell_emit.grid_epoch_base.clone()),
        frozen_epoch
    );
    assert_eq!(harness.with_record(|record| record.head_seq), frozen_head);
    let painted = harness.fulls().pop().unwrap();
    assert_eq!(
        (painted.stream_id.as_str(), painted.cols, painted.rows),
        (STREAM_B, 18, 8)
    );
    assert!(Harness::row_text(&painted, 0).contains("BEFORE-TRAP"));
    assert!(Harness::row_text(&painted, 1).contains("AFTER-TRAP"));
}

#[tokio::test]
async fn a_lost_ack_whose_boundary_is_not_retained_reports_a_reprovable_core() {
    let harness = Harness::scripted(RioCore::new(COLS, ROWS));
    *held(&harness.keeper.history) = Some(SurvivorHistory {
        records: Vec::new(),
        head_seq: 0,
        base_cols: COLS,
        base_rows: ROWS,
    });
    held(&harness.keeper.script).push_back(Answer::Lost);
    let result = harness.enable(STREAM_A, REPROVED_COLS, REPROVED_ROWS).await;
    assert!(
        matches!(&result, WorkerStreamResult::Ambiguous { failure: Failure::CoreFailed, reason, .. }
        if reason == "ordered resize boundary was not retained"),
        "{result:?}"
    );
    assert!(!harness.core_valid());
    assert!(gate_released(&harness));
}
