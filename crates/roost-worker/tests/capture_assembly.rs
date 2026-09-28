//! Capture assembly: the absolute history rows the browser named are read back
//! at their exact indices THROUGH the payload nesting, rows below the floor or
//! past the total are reported evicted/unavailable rather than omitted, and an
//! over-budget grid is never scanned. Ports `apps/worker/tests/terminal/
//! terminal-capture-assembly.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod capture_support;
mod terminal_stream_support;

use capture_support::{
    AT_MS, CaptureHarness, browser_payload, capture_command, live_full_frame, read_bundle,
};
use roost_protocol::terminal_capture::TERMINAL_CAPTURE_LIMITS;
use roost_protocol::terminal_capture::bundle::TerminalWorkerComparison;
use roost_term::{AlacrittyCore, scrollback_origin};
use roost_worker::browser_commands::diagnostics::DiagnosticReports;
use serde_json::{Value, json};
use terminal_stream_support::{COLS, Harness, ROWS, SESSION, STREAM_A};

fn scrollback_total(harness: &CaptureHarness) -> u64 {
    harness.with_record(|record| {
        let core = record.terminal_core.as_ref();
        scrollback_origin(core, record.cell_emit.scrollback_origin).unwrap()
            + core.scrollback_count() as u64
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn browser_named_history_rows_are_read_back_at_their_absolute_indices() {
    let harness = CaptureHarness::new("assembly-rows");
    harness.stream.enable(STREAM_A, COLS, ROWS).await;
    harness.start();
    for line in 0..30 {
        harness.deliver(format!("L{line}\r\n").as_bytes());
    }
    let total = scrollback_total(&harness);
    assert!(total > 10, "{total}");

    let mut command = capture_command();
    let gaps = [((total + 4).to_string(), (total + 9).to_string())];
    command.browser_evidence_json =
        browser_payload(&command.capture_id, &[5, 6, 7], &gaps).to_string();
    let result = harness.recorder.capture(command).await;
    assert_eq!(result.error, None);
    let bundle = read_bundle(result.path.as_deref().unwrap()).await;
    // Browser evidence WAS supplied, so the bundle carries the browser section.
    assert_eq!(
        (
            bundle["browser"]["captured_at_ms"].clone(),
            bundle["browser"]["layer"].clone()
        ),
        (json!(AT_MS), json!("browser"))
    );
    let worker = &bundle["worker"];
    assert_eq!(
        worker["history_ranges"],
        json!([
            { "start": "5", "end": "8", "status": "present", "rows": 3 },
            { "start": (total + 4).to_string(), "end": (total + 9).to_string(), "status": "unavailable", "rows": 0 },
        ])
    );
    let indices: Vec<Value> = worker["history_rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["index"].clone())
        .collect();
    assert_eq!(indices, vec![json!(5), json!(6), json!(7)]);
    // The named rows carry their real painted text, which is the point.
    assert_eq!(worker["history_rows"][0]["spans"][0]["text"], json!("L5"));
    // The browser authored the trigger; the worker must not overwrite it.
    let trigger = &bundle["trigger"];
    assert_eq!(
        (
            trigger["reason"].clone(),
            trigger["origin"].clone(),
            trigger["detail"].clone(),
            trigger["occurrence_count"].clone()
        ),
        (
            json!("history_identity"),
            json!("browser"),
            json!("duplicate_history_index"),
            json!(3)
        )
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_range_below_the_retained_floor_is_reported_evicted_not_omitted() {
    let harness = CaptureHarness::new("assembly-floor");
    harness.stream.enable(STREAM_A, COLS, ROWS).await;
    harness.start();
    harness.deliver(b"only-line\r\n");
    // An origin above zero: the browser holds rows this core no longer has.
    harness.with_record(|record| record.cell_emit.scrollback_origin = 40);

    let mut command = capture_command();
    command.browser_evidence_json = browser_payload(&command.capture_id, &[3], &[]).to_string();
    let result = harness.recorder.capture(command).await;
    let worker = read_bundle(result.path.as_deref().unwrap()).await["worker"].clone();
    assert_eq!(
        worker["history_ranges"],
        json!([{ "start": "3", "end": "4", "status": "evicted", "rows": 0 }])
    );
    assert_eq!(worker["history_rows"], json!([]));
    assert_eq!(worker["scrollback_origin"], json!("40"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_over_budget_grid_is_never_scanned_and_says_so_in_coverage() {
    let harness = CaptureHarness::over(
        "assembly-grid",
        Harness::scripted(AlacrittyCore::new(200, 200)),
    );
    harness.stream.enable(STREAM_A, 200, 200).await;
    harness.start();
    let tap = harness.recorder.tap();
    harness.with_record(|record| tap.accepted_emission(record, Some(live_full_frame(record, 1))));

    assert!(200 * 200 > TERMINAL_CAPTURE_LIMITS.core_sample_max_cells);
    let (sampled, skipped_grid, comparison) =
        harness
            .recorder
            ._with_terminal_recorder(SESSION, |recorder| {
                let recorder = recorder.unwrap();
                (
                    recorder.sampling.sampled,
                    recorder.sampling.skipped_grid,
                    recorder.emissions[0].record.comparison,
                )
            });
    assert_eq!(
        (sampled, skipped_grid, comparison),
        (0, 1, TerminalWorkerComparison::BudgetSkipped)
    );

    let result = harness.recorder.capture(capture_command()).await;
    let bundle = read_bundle(result.path.as_deref().unwrap()).await;
    assert_eq!(bundle["coverage"]["core_comparison"], json!("unavailable"));
    assert_eq!(
        bundle["coverage"]["core_comparison_reasons"],
        json!(["grid_budget_exceeded"])
    );
    assert_eq!(bundle["worker"]["core_samples"], json!([]));
}
