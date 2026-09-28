//! The worker terminal stream contract: validation, generation minting, the
//! baseline a commit owes, snapshot repair, and the in-place resize at the
//! keeper boundary with its truthful outcomes. Ports
//! `apps/worker/tests/terminal/terminal-stream-{state,resize}.test.ts` over
//! `session::terminal_control` / `terminal_txn` / `resize`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod terminal_stream_support;

use roost_keeper::client_resize::ResizeRejectReason;
use roost_protocol::wire::coord_worker::{
    TerminalStreamFailureKind as Failure, TerminalWritePhase,
};
use roost_term::AlacrittyCore;
use roost_worker::session::terminal_state::WorkerStreamResult;
use terminal_stream_support::{
    Answer, COLS, Harness, ROWS, STREAM_A, STREAM_B, STREAM_C, core_with, held,
};

fn failure(result: &WorkerStreamResult) -> (Option<Failure>, TerminalWritePhase, u64) {
    (
        result.failure(),
        result.phase(),
        result.channel_resize_seq(),
    )
}

#[tokio::test]
async fn invalid_desires_are_refused_before_any_keeper_write() {
    let harness = Harness::scripted(AlacrittyCore::new(COLS, ROWS));
    let v7 = "00000000-0000-7000-8000-00000000000a";
    let cases = [
        (harness.intent(v7, true, 12, 6), "stream_id must be a UUID"),
        (
            harness.intent(STREAM_A, false, 12, 6),
            "disabled terminal stream must have zero geometry",
        ),
        (
            harness.intent(STREAM_A, true, 0, 6),
            "enabled geometry must be within 1..256 cols and 1..256 rows",
        ),
        (
            harness.intent(STREAM_A, true, 257, 6),
            "enabled geometry must be within 1..256 cols and 1..256 rows",
        ),
    ];
    for (intent, reason) in cases {
        let result = harness.manager.apply_terminal_stream_state(intent).await;
        assert!(
            matches!(&result, WorkerStreamResult::Rejected { reason: said, .. } if said == reason),
            "{result:?}"
        );
        assert_eq!(
            failure(&result),
            (
                Some(Failure::InvalidRequest),
                TerminalWritePhase::PreWrite,
                0
            )
        );
    }
    let mut blank = harness.intent(STREAM_A, true, 12, 6);
    blank.request_id.clear();
    let result = harness.manager.apply_terminal_stream_state(blank).await;
    assert!(
        matches!(&result, WorkerStreamResult::Rejected { reason, .. } if reason == "request_id is invalid")
    );
    let mut elsewhere = harness.intent(STREAM_A, true, 12, 6);
    elsewhere.session_id =
        roost_protocol::wire::brand::SessionId::try_from("99999999-2222-4333-8444-555555555555")
            .unwrap();
    let result = harness.manager.apply_terminal_stream_state(elsewhere).await;
    assert_eq!(result.failure(), Some(Failure::SessionNotLive));
    assert!(held(&harness.keeper.resized).is_empty());
    assert!(
        harness
            .manager
            .terminal_stream_facts(terminal_stream_support::channel())
            .is_none()
    );
}

#[tokio::test]
async fn a_same_geometry_commit_installs_one_baseline_and_a_repeat_is_the_same_answer() {
    let harness = Harness::scripted(core_with(COLS, ROWS, b"BASELINE"));
    let first = harness.enable(STREAM_A, COLS, ROWS).await;
    assert_eq!(
        first,
        WorkerStreamResult::Committed {
            stream_id: STREAM_A.into(),
            enabled: true,
            cols: 12,
            rows: 6,
            channel_resize_seq: 0,
            resized: false
        }
    );
    let fulls = harness.fulls();
    assert_eq!(fulls.len(), 1, "the commit owes exactly one baseline");
    assert_eq!(
        (fulls[0].stream_id.as_str(), fulls[0].seq, fulls[0].base_seq),
        (STREAM_A, 1, 0)
    );
    assert!(Harness::row_text(&fulls[0], 0).contains("BASELINE"));
    // v2 returns `current.operation`: the same stream id and payload is the same transaction.
    assert_eq!(harness.enable(STREAM_A, COLS, ROWS).await, first);
    assert_eq!(harness.fulls().len(), 1);
    let conflicting = harness.enable(STREAM_A, 10, 3).await;
    assert!(
        matches!(&conflicting, WorkerStreamResult::Rejected { reason, .. } if reason == "stream_id was reused with a conflicting payload")
    );
    assert!(
        held(&harness.keeper.resized).is_empty(),
        "no geometry changed, so nothing reached the keeper"
    );

    let disabled = harness
        .manager
        .apply_terminal_stream_state(harness.intent(STREAM_C, false, 0, 0))
        .await;
    assert!(matches!(
        disabled,
        WorkerStreamResult::Committed {
            cols: 0,
            rows: 0,
            enabled: false,
            ..
        }
    ));
    assert_eq!(
        harness
            .manager
            .current_terminal_stream_id(&terminal_stream_support::session_id()),
        None
    );
}

#[tokio::test]
async fn a_renewal_over_the_same_grid_keeps_its_epoch_and_restarts_its_sequence() {
    let harness = Harness::scripted(core_with(
        COLS,
        ROWS,
        b"H0\r\nH1\r\nH2\r\nH3\r\nH4\r\nH5\r\nH6\r\nH7\r\n",
    ));
    harness.enable(STREAM_A, COLS, ROWS).await;
    harness.enable(STREAM_B, COLS, ROWS).await;
    let fulls = harness.fulls();
    assert_eq!(fulls.len(), 2);
    assert_eq!(
        (fulls[1].stream_id.as_str(), fulls[1].seq, fulls[1].base_seq),
        (STREAM_B, 1, 0)
    );
    assert_eq!(
        fulls[1].grid_epoch, fulls[0].grid_epoch,
        "a stream generation owns sequence space, not grid identity"
    );
}

#[tokio::test]
async fn a_snapshot_request_rebaselines_only_the_current_stream() {
    let harness = Harness::scripted(core_with(COLS, ROWS, b"BASE"));
    harness.enable(STREAM_A, COLS, ROWS).await;
    let session = terminal_stream_support::session_id();
    harness
        .manager
        .request_terminal_snapshot(&session, STREAM_B);
    assert_eq!(
        harness.fulls().len(),
        1,
        "a request for another stream is ignored"
    );
    harness
        .manager
        .request_terminal_snapshot(&session, STREAM_A);
    let fulls = harness.fulls();
    assert_eq!(fulls.len(), 2);
    assert_eq!(
        (fulls[1].stream_id.as_str(), fulls[1].seq, fulls[1].base_seq),
        (STREAM_A, 2, 0)
    );
}

#[tokio::test]
async fn shrink_and_grow_resize_the_same_core_at_the_keeper_boundary() {
    let rows: Vec<String> = (0..ROWS).map(|row| format!("P{row}-STATIC")).collect();
    let mut paint = Vec::new();
    for (row, text) in rows.iter().enumerate() {
        paint.extend_from_slice(format!("\x1b[{};1H{text}", row + 1).as_bytes());
    }
    let harness = Harness::scripted(core_with(COLS, ROWS, &paint));
    let before = harness
        .with_record(|record| std::ptr::addr_of!(*record.terminal_core) as *const u8 as usize);
    assert!(matches!(
        harness.enable(STREAM_A, COLS, ROWS).await,
        WorkerStreamResult::Committed { resized: false, .. }
    ));
    let shrink = harness.enable(STREAM_B, 10, 3).await;
    assert_eq!(
        shrink,
        WorkerStreamResult::Committed {
            stream_id: STREAM_B.into(),
            enabled: true,
            cols: 10,
            rows: 3,
            channel_resize_seq: 1,
            resized: true
        }
    );
    let grow = harness.enable(STREAM_C, COLS, ROWS).await;
    assert!(matches!(
        grow,
        WorkerStreamResult::Committed {
            channel_resize_seq: 2,
            resized: true,
            cols: 12,
            rows: 6,
            ..
        }
    ));
    let after = harness
        .with_record(|record| std::ptr::addr_of!(*record.terminal_core) as *const u8 as usize);
    assert_eq!(before, after, "the core is resized in place, never rebuilt");
    assert_eq!(
        *held(&harness.keeper.resized),
        vec![(43, 1, 10, 3), (43, 2, 12, 6)]
    );
    let fulls = harness.fulls();
    let shape: Vec<_> = fulls
        .iter()
        .map(|frame| (frame.stream_id.as_str(), frame.seq, frame.cols, frame.rows))
        .collect();
    assert_eq!(
        shape,
        vec![
            (STREAM_A, 1, 12, 6),
            (STREAM_B, 1, 10, 3),
            (STREAM_C, 1, 12, 6)
        ]
    );
    assert_ne!(
        fulls[1].grid_epoch, fulls[0].grid_epoch,
        "a proven resize is a new grid"
    );
    assert!(
        Harness::row_text(&fulls[1], 2).contains("P5-STATI"),
        "the cursor's rows survive the shrink"
    );
    assert_eq!(
        harness.with_record(|record| (record.terminal_core.cols(), record.terminal_core.rows())),
        (12, 6)
    );
}

#[tokio::test]
async fn a_keeper_refusal_after_the_write_is_rejected_as_written_and_moves_nothing() {
    let harness = Harness::scripted(AlacrittyCore::new(COLS, ROWS));
    held(&harness.keeper.script).extend([
        Answer::Refuse(ResizeRejectReason::StaleSequence),
        Answer::Refuse(ResizeRejectReason::ChannelExited),
        Answer::NotWritten,
    ]);
    let stale = harness.enable(STREAM_A, 20, 10).await;
    assert_eq!(
        failure(&stale),
        (
            Some(Failure::RetryablePreWrite),
            TerminalWritePhase::Written,
            1
        )
    );
    assert!(
        matches!(&stale, WorkerStreamResult::Rejected { reason, .. } if reason == "keeper rejected terminal resize: stale_sequence")
    );
    let exited = harness.enable(STREAM_B, 20, 10).await;
    assert_eq!(
        failure(&exited),
        (
            Some(Failure::SessionNotLive),
            TerminalWritePhase::Written,
            2
        )
    );
    let unwritten = harness.enable(STREAM_C, 20, 10).await;
    assert_eq!(
        failure(&unwritten),
        (
            Some(Failure::RetryablePreWrite),
            TerminalWritePhase::PreWrite,
            2
        )
    );
    assert_eq!(
        harness.manager.channel_resize_seq(43),
        2,
        "an unwritten request spends no sequence"
    );
    assert_eq!(
        harness.with_record(|record| (record.terminal_core.cols(), record.terminal_core.rows())),
        (COLS, ROWS)
    );
    assert!(
        held(&harness.emitter)
            .gate_suppression(terminal_stream_support::channel())
            .is_none(),
        "every settled boundary hands its gate back"
    );
}

#[tokio::test]
async fn a_lost_ack_is_recovered_in_place_from_the_ordered_history() {
    use roost_keeper::history::HistoryRecord;
    use roost_worker::session::keeper_channels::SurvivorHistory;
    let harness = Harness::scripted(core_with(COLS, ROWS, b"prompt$ "));
    harness.enable(STREAM_A, COLS, ROWS).await;
    let head = harness.with_record(|record| record.head_seq);
    *held(&harness.keeper.history) = Some(SurvivorHistory {
        records: vec![
            HistoryRecord::Output {
                seq: 1,
                bytes: b"prompt$ ".to_vec(),
            },
            HistoryRecord::Resize {
                seq: 1,
                cols: 20,
                rows: 10,
            },
        ],
        head_seq: head + 8,
        base_cols: COLS,
        base_rows: ROWS,
    });
    held(&harness.keeper.script).push_back(Answer::Lost);
    let result = harness.enable(STREAM_B, 20, 10).await;
    assert!(
        matches!(
            result,
            WorkerStreamResult::Committed {
                resized: true,
                channel_resize_seq: 1,
                ..
            }
        ),
        "{result:?}"
    );
    assert_eq!(
        harness.with_record(|record| (record.terminal_core.cols(), record.terminal_core.rows())),
        (20, 10)
    );
    assert!(harness.core_valid());
}
