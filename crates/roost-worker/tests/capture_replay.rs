//! A bundle this worker writes actually replays: armed BEFORE the fresh core's
//! first byte, the real data path retains every chunk as an unbroken offset
//! chain, a keeper-acknowledged resize lands a real boundary offset, and a
//! fresh core fed the retained bytes alone rebuilds the same screen — one
//! footer, not two. Ports `apps/worker/tests/terminal/terminal-capture-replay.test.ts`
//! (the replay script itself is not the worker's; its core replay is inlined).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod capture_support;
mod terminal_stream_support;

use std::sync::Arc;

use base64::Engine as _;
use capture_support::{CaptureHarness, capture_command, read_bundle};
use roost_protocol::cell::{CellGridFrame, spans_text};
use roost_protocol::terminal_capture::TERMINAL_CAPTURE_LIMITS;
use roost_protocol::terminal_capture::view::{canonical_view_of_frame, compare_canonical_views};
use roost_term::{AlacrittyCore, TerminalCore, grid_to_cell_frame};
use roost_worker::browser_commands::diagnostics::DiagnosticReports;
use roost_worker::session::terminal_state::WorkerStreamResult;
use serde_json::{Value, json};
use terminal_stream_support::{COLS, ROWS, SESSION, STREAM_A, STREAM_B, held};

const OLD_FOOTER: &str = "FOOTER-12s";
const NEW_FOOTER: &str = "FOOTER-14s";
const SHRUNK_ROWS: u16 = 4;

fn emit(harness: &CaptureHarness, force: bool) {
    let emitter = Arc::clone(&harness.stream.emitter);
    harness.with_record(|record| {
        held(&emitter).emit_cell_frame(record, force, 1_700_000_000_000);
    });
}

fn viewport(core: &dyn TerminalCore) -> CellGridFrame {
    let origin = core.scrollback_origin(0).unwrap();
    grid_to_cell_frame(core, 1, "replay:0", STREAM_A, Some(0), origin)
}

/// Feed the retained raw chain to a fresh core, resizing it at each accepted
/// boundary offset — the parser replay a bundle exists to make possible.
fn replay(section: &Value) -> AlacrittyCore {
    let mut core = AlacrittyCore::new(COLS, ROWS);
    let resizes: Vec<(u64, u16, u16)> = section["resizes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|resize| resize["outcome"] == json!("accepted"))
        .map(|resize| {
            let at = resize["boundary_offset"].as_str().unwrap().parse().unwrap();
            (
                at,
                resize["to"]["cols"].as_u64().unwrap() as u16,
                resize["to"]["rows"].as_u64().unwrap() as u16,
            )
        })
        .collect();
    let mut offset = 0;
    for record in section["raw"].as_array().unwrap() {
        assert_eq!(
            record["start_offset"]
                .as_str()
                .unwrap()
                .parse::<u64>()
                .unwrap(),
            offset,
            "an unbroken chain"
        );
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(record["base64"].as_str().unwrap())
            .unwrap();
        core.write(&bytes);
        offset += bytes.len() as u64;
        for (at, cols, rows) in &resizes {
            if *at == offset {
                core.resize(*cols, *rows);
            }
        }
    }
    core
}

#[tokio::test(flavor = "multi_thread")]
async fn a_recording_armed_at_the_first_byte_replays_to_one_overwritten_footer() {
    let harness = CaptureHarness::new("replay-exact");
    harness.stream.enable(STREAM_A, COLS, ROWS).await;
    // Armed BEFORE the core parsed a byte: the only state exact replay needs.
    assert_eq!(harness.with_record(|record| record.head_seq), 0);
    harness.start();

    harness.deliver(format!("\x1b[{ROWS};1H{OLD_FOOTER}").as_bytes());
    emit(&harness, true);
    // One second later the application rewrites the SAME row: bare CR.
    harness
        .recorder
        ._with_terminal_recorder(SESSION, |recorder| {
            let recorder = recorder.unwrap();
            recorder.last_sample_mono = None;
            recorder.sample_suppressed_until_mono = None;
        });
    harness.deliver(format!("\r{NEW_FOOTER}").as_bytes());
    emit(&harness, false);

    let resized = harness.stream.enable(STREAM_B, COLS, SHRUNK_ROWS).await;
    assert!(
        matches!(resized, WorkerStreamResult::Committed { resized: true, .. }),
        "{resized:?}"
    );
    let boundary = harness.with_record(|record| record.head_seq);

    let result = harness.recorder.capture(capture_command()).await;
    assert_eq!(result.error, None);
    let bundle = read_bundle(result.path.as_deref().unwrap()).await;
    let section = &bundle["worker"];
    let segment = &section["segments"][0];
    assert_eq!(segment["open_offset"], json!("0"));
    let offsets: Vec<(Value, Value)> = section["raw"]
        .as_array()
        .unwrap()
        .iter()
        .map(|raw| (raw["start_offset"].clone(), raw["end_offset"].clone()))
        .collect();
    assert_eq!(
        offsets,
        vec![(json!("0"), json!("16")), (json!("16"), json!("27"))]
    );
    assert!(
        section["raw"]
            .as_array()
            .unwrap()
            .iter()
            .all(|raw| raw["segment_id"] == segment["segment_id"])
    );
    assert_eq!(bundle["coverage"]["core_replay"], json!("complete"));

    // The boundary is the keeper-acknowledged raw offset, never a request time.
    let resize = section["resizes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|resize| resize["outcome"] == json!("accepted"))
        .unwrap();
    assert_eq!(resize["boundary_offset"], json!(boundary.to_string()));
    assert_eq!(resize["to"], json!({ "cols": COLS, "rows": SHRUNK_ROWS }));

    // The retained bytes alone rebuild the worker's own screen exactly.
    let replayed = viewport(&replay(section));
    let live = harness.with_record(|record| viewport(record.terminal_core.as_ref()));
    let (replayed_view, live_view) = (
        canonical_view_of_frame(&replayed).unwrap(),
        canonical_view_of_frame(&live).unwrap(),
    );
    assert_eq!(compare_canonical_views(&replayed_view, &live_view), None);
    let rows: Vec<String> = replayed
        .viewport_rows
        .iter()
        .map(|row| spans_text(&row.spans))
        .collect();
    let count = |footer: &str| rows.iter().filter(|row| row.contains(footer)).count();
    assert_eq!((count(OLD_FOOTER), count(NEW_FOOTER)), (0, 1), "{rows:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_recording_armed_after_the_first_byte_refuses_to_claim_exact_replay() {
    let harness = CaptureHarness::new("replay-late");
    harness.stream.enable(STREAM_A, COLS, ROWS).await;
    // The real production situation: the shell already initialized itself.
    harness.deliver(b"\x1b[1;1Halready-here");
    assert!(harness.with_record(|record| record.head_seq) > 0);
    harness.start();
    harness.deliver(format!("\x1b[{ROWS};1H{NEW_FOOTER}").as_bytes());
    emit(&harness, true);

    let bundle = read_bundle(
        harness
            .recorder
            .capture(capture_command())
            .await
            .path
            .as_deref()
            .unwrap(),
    )
    .await;
    assert_eq!(bundle["coverage"]["core_replay"], json!("partial"));
    assert!(
        bundle["coverage"]["core_replay_reasons"]
            .as_array()
            .unwrap()
            .contains(&json!("missing_initial_prefix"))
    );
    assert_ne!(bundle["worker"]["segments"][0]["open_offset"], json!("0"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_evicted_raw_prefix_refuses_to_claim_exact_replay() {
    let harness = CaptureHarness::new("replay-evicted");
    harness.stream.enable(STREAM_A, COLS, ROWS).await;
    harness.start();
    // Past the retained-record bound, so the pools drop the oldest raw records
    // from the FRONT — the prefix the parser state depends on.
    for idx in 0..TERMINAL_CAPTURE_LIMITS.layer_entries + 4 {
        harness.deliver(format!("\x1b[1;1Hrow{idx}").as_bytes());
    }
    emit(&harness, true);
    assert!(
        !harness
            .recorder
            ._with_terminal_recorder(SESSION, |recorder| recorder
                .unwrap()
                .retention
                .raw_prefix_complete)
    );

    let bundle = read_bundle(
        harness
            .recorder
            .capture(capture_command())
            .await
            .path
            .as_deref()
            .unwrap(),
    )
    .await;
    // The bundle NAMES the evicted range rather than presenting a short chain
    // as a complete one.
    let omissions = bundle["worker"]["omissions"].as_array().unwrap();
    let raw = omissions
        .iter()
        .find(|omission| omission["name"] == json!("worker.raw"))
        .unwrap();
    assert_eq!(
        (raw["reason"].clone(), raw["dropped_count"].clone()),
        (json!("raw_prefix_evicted"), json!(4))
    );
    assert!(
        bundle["coverage"]["core_replay_reasons"]
            .as_array()
            .unwrap()
            .contains(&json!("raw_prefix_evicted"))
    );
}
