//! The scheduler's own decisions, with no v2 test behind them: coalescing, the
//! paint hold, the refused-paint repair, and the claim that `now_ms` is the
//! only clock the decision path reads.
//!
//! The last of those is the load-bearing one. Every other guarantee here — a
//! batch that cannot be re-folded, a hold that cannot be bypassed, a wait a
//! caller can predict — is worth nothing if a decision can also depend on a
//! clock the test does not control, so the source is scanned for one.

mod render_scheduler_support;

use std::path::PathBuf;

use render_scheduler_support::{RecordingRenderer, delta_frame, flush_frame, full_frame, offer};
use roost_web_terminal::scheduler::{ApplyMode, EnqueueDecision, FrameDecision, RenderScheduler};
use roost_web_terminal::{RENDERER_HOLD_LINK, RENDERER_HOLD_SELECTION, ReconcileBlockReason};

#[test]
fn two_frames_inside_one_browser_frame_produce_one_paint() {
    let mut scheduler = RenderScheduler::new();
    let mut renderer = RecordingRenderer::default();
    scheduler.set_foreground(true);
    let baseline = full_frame(1, "A");
    offer(&mut scheduler, &baseline, baseline.clone(), 0);
    flush_frame(&mut scheduler, &mut renderer, 4, 0);

    let first = offer(&mut scheduler, &delta_frame(2, "B"), full_frame(2, "B"), 8);
    let second = offer(&mut scheduler, &delta_frame(3, "C"), full_frame(3, "C"), 10);
    assert!(
        first.armed,
        "the arrival that opens a batch is the one that arms"
    );
    assert!(
        !second.armed,
        "a second arrival inside the armed frame must not ask for a second frame"
    );
    assert_eq!(
        second.decision,
        EnqueueDecision::Coalesced {
            mode: ApplyMode::DeltaBatch,
            batch_frames: 2,
            appended_rows: 0,
        }
    );

    let paint = flush_frame(&mut scheduler, &mut renderer, 16, 0);
    assert_eq!(paint.mode, Some(ApplyMode::DeltaBatch));
    assert_eq!(paint.batch_frames, Some(2));
    assert_eq!(
        paint.queue_delay_ms,
        Some(8),
        "a batch's wait is measured from the arrival that OPENED it"
    );
    assert_eq!(
        renderer.delta_seqs(),
        vec![vec![2, 3]],
        "two arrivals, one paint, one batch of two"
    );
    assert_eq!(renderer.full_seqs(), vec![1]);
}

#[test]
fn a_held_renderer_names_its_hold_and_keeps_the_batch() {
    let mut scheduler = RenderScheduler::new();
    let mut renderer = RecordingRenderer::default();
    scheduler.set_foreground(true);
    let baseline = full_frame(1, "A");
    offer(&mut scheduler, &baseline, baseline.clone(), 0);
    flush_frame(&mut scheduler, &mut renderer, 4, 0);
    offer(&mut scheduler, &delta_frame(2, "B"), full_frame(2, "B"), 8);

    for (hold_mask, reason) in [
        (RENDERER_HOLD_LINK, ReconcileBlockReason::LinkHold),
        (RENDERER_HOLD_SELECTION, ReconcileBlockReason::SelectionHold),
        (
            RENDERER_HOLD_SELECTION | RENDERER_HOLD_LINK,
            ReconcileBlockReason::SelectionAndLinkHold,
        ),
    ] {
        assert!(
            scheduler.schedule_browser_frame(),
            "the caller re-arms after a hold kept the batch"
        );
        assert_eq!(
            scheduler.on_frame_fired(12, hold_mask),
            FrameDecision::Held { reason },
            "the reason a held paint is skipped is the one the reconcile snapshot \
             reports for a held pane, read off the same mask"
        );
    }
    assert!(
        renderer.delta_seqs().is_empty(),
        "a held pane paints nothing"
    );
    assert_eq!(
        scheduler.pending_mode(),
        Some(ApplyMode::DeltaBatch),
        "and the batch survives every hold, so the release paints it"
    );

    let released = flush_frame(&mut scheduler, &mut renderer, 400, 0);
    assert_eq!(released.mode, Some(ApplyMode::DeltaBatch));
    assert_eq!(renderer.delta_seqs(), vec![vec![2]]);
}

#[test]
fn a_refused_paint_is_repaired_as_a_fallback_full_and_keeps_its_queue_clock() {
    let mut scheduler = RenderScheduler::new();
    let mut renderer = RecordingRenderer::default();
    scheduler.set_foreground(true);
    let baseline = full_frame(1, "A");
    offer(&mut scheduler, &baseline, baseline.clone(), 0);
    flush_frame(&mut scheduler, &mut renderer, 4, 0);
    offer(&mut scheduler, &delta_frame(2, "B"), full_frame(2, "B"), 8);

    renderer.refuse_next = true;
    let refused = flush_frame(&mut scheduler, &mut renderer, 12, 0);
    assert!(refused.painted, "the batch WAS handed to the renderer");
    assert!(renderer.delta_seqs().is_empty());
    assert_eq!(
        scheduler.pending_mode(),
        Some(ApplyMode::FallbackFull),
        "a refused delta batch is repaired as the canonical full it named"
    );
    assert_eq!(
        scheduler.reconciled_watermark(),
        None,
        "a refused paint must not advance the watermark the next delta extends"
    );
    assert!(
        scheduler.needs_browser_frame(),
        "and the repaired batch still owes a paint"
    );

    let repair = flush_frame(&mut scheduler, &mut renderer, 16, 0);
    assert_eq!(repair.mode, Some(ApplyMode::FallbackFull));
    assert_eq!(
        repair.batch_frames,
        Some(2),
        "the arrival count carries over"
    );
    assert_eq!(
        repair.queue_delay_ms,
        Some(8),
        "so does the queue clock: the repair is the same batch, waited on longer"
    );
    assert_eq!(renderer.full_seqs(), vec![2]);
    assert_eq!(
        scheduler.reconciled_watermark().and_then(|mark| mark.seq),
        Some(2)
    );
    assert!(
        !repair.delivery.expect("the repair delivered").had_wire_full,
        "a repaired delta batch descends from no wire full"
    );
}

#[test]
fn a_batch_reports_exactly_the_wait_the_caller_supplied() {
    // The same frame sequence three times over, differing only in the clock the
    // caller hands over. Identical decisions, and a wait that is the caller's
    // own arithmetic and nothing else — including at the zero-wait boundary.
    for (queued_at_ms, fired_at_ms) in [(0u64, 4u64), (1_000, 1_007), (5_000, 5_000)] {
        let mut scheduler = RenderScheduler::new();
        let mut renderer = RecordingRenderer::default();
        scheduler.set_foreground(true);
        let baseline = full_frame(1, "A");
        offer(&mut scheduler, &baseline, baseline.clone(), queued_at_ms);
        let base_paint = flush_frame(&mut scheduler, &mut renderer, fired_at_ms, 0);
        offer(
            &mut scheduler,
            &delta_frame(2, "B"),
            full_frame(2, "B"),
            queued_at_ms.saturating_add(1),
        );
        let paint = flush_frame(&mut scheduler, &mut renderer, queued_at_ms + 9, 0);

        assert_eq!(base_paint.mode, Some(ApplyMode::WireFull));
        assert_eq!(paint.mode, Some(ApplyMode::DeltaBatch));
        assert_eq!(paint.queue_delay_ms, Some(8));
        assert_eq!(renderer.full_seqs(), vec![1]);
        assert_eq!(renderer.delta_seqs(), vec![vec![2]]);
    }
}

#[test]
fn the_decision_path_names_no_clock_and_no_dom_binding() {
    // The determinism claim is that `now_ms` is the ONLY time these modules
    // read. A `SystemTime`, an `Instant` or a host binding would make every
    // decision here depend on something a test cannot pin, so the source is
    // scanned for all three. Mirrors the scan in
    // `crates/roost-client-core/tests/core_without_a_browser.rs`.
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let sources = [
        crate_root.join("src/scheduler.rs"),
        crate_root.join("src/scheduler/cursor_poll.rs"),
        crate_root.join("src/scheduler/frame_gate.rs"),
        crate_root.join("src/scheduler/frames.rs"),
    ];
    let forbidden = [
        "SystemTime",
        "std::time",
        "Instant",
        "Date::now",
        "performance",
        "web_sys",
        "wasm_bindgen",
        "js_sys",
    ];
    let mut offenders = Vec::new();
    for source in &sources {
        let text = std::fs::read_to_string(source).expect("the scheduler sources are in the crate");
        for (index, line) in text.lines().enumerate() {
            for token in forbidden {
                if line.contains(token) {
                    offenders.push(format!(
                        "{}:{} names `{token}`",
                        source.display(),
                        index + 1
                    ));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "the render scheduler must read no clock and reach no browser type:\n{}",
        offenders.join("\n")
    );
}
