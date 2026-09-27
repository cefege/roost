//! The browser-frame boundary for terminal DOM delivery, ported from
//! `apps/web/tests/terminalRenderScheduler.test.ts` and
//! `apps/web/tests/terminalStreamRenderScheduler.test.ts`.
//!
//! A caller-supplied clock replaces the harness's controllable
//! `requestAnimationFrame`, so bounded batching, parking, stale-full refusal and
//! fallback repair are all decided without a browser, a socket or a replica.

mod render_scheduler_support;

use render_scheduler_support::{
    RecordedDelivery, RecordingRenderer, delta_frame, flush_frame, full_frame, offer, row_shell,
};
use roost_web_terminal::scheduler::{ApplyMode, EnqueueDecision, FrameDecision, RenderScheduler};

#[test]
fn folds_contiguous_deltas_into_one_sparse_paint() {
    let mut scheduler = RenderScheduler::new();
    let mut renderer = RecordingRenderer::default();
    scheduler.set_foreground(true);
    let mut deliveries: Vec<RecordedDelivery> = Vec::new();

    let first = full_frame(1, "A");
    let opened = offer(&mut scheduler, &first, first.clone(), 0);
    assert_eq!(
        opened.decision,
        EnqueueDecision::Coalesced {
            mode: ApplyMode::WireFull,
            batch_frames: 1,
            appended_rows: 0,
        }
    );
    assert!(opened.armed, "the first arrival arms the browser frame");
    deliveries.push(
        flush_frame(&mut scheduler, &mut renderer, 4, 0)
            .delivery
            .expect("the baseline painted"),
    );

    offer(&mut scheduler, &delta_frame(2, "B"), full_frame(2, "B"), 8);
    deliveries.push(
        flush_frame(&mut scheduler, &mut renderer, 12, 0)
            .delivery
            .expect("the lone delta painted"),
    );
    assert_eq!(renderer.delta_seqs(), vec![vec![2]]);

    let mut fourth_frame = delta_frame(4, "D");
    fourth_frame.cursor_keys_app = true;
    fourth_frame.bracketed_paste = true;
    let mut fourth_canonical = full_frame(4, "D");
    fourth_canonical.cursor_keys_app = true;
    fourth_canonical.bracketed_paste = true;

    let third = offer(&mut scheduler, &delta_frame(3, "C"), full_frame(3, "C"), 16);
    assert_eq!(
        third.decision,
        EnqueueDecision::Coalesced {
            mode: ApplyMode::DeltaBatch,
            batch_frames: 1,
            appended_rows: 0,
        }
    );
    assert!(
        third.armed,
        "the arrival that opens a batch is the one that arms"
    );

    let fourth = offer(&mut scheduler, &fourth_frame, fourth_canonical, 18);
    assert!(
        !fourth.armed,
        "a second arrival inside one browser frame must not ask for a second"
    );
    deliveries.push(
        flush_frame(&mut scheduler, &mut renderer, 20, 0)
            .delivery
            .expect("the folded batch painted"),
    );

    assert_eq!(renderer.full_seqs(), vec![1]);
    assert_eq!(renderer.delta_seqs(), vec![vec![2], vec![3, 4]]);
    let summaries: Vec<_> = deliveries.iter().map(RecordedDelivery::summary).collect();
    assert_eq!(
        summaries,
        vec![
            (1, true, false, false, 1, true, false),
            (2, false, false, false, 2, true, false),
            (4, false, true, true, 4, true, false),
        ],
        "a delivery names the LAST delta of its batch, against the canonical it \
         was delivered for, and carries that delta's terminal modes"
    );
}

#[test]
fn preserves_a_queued_rebaseline_full_through_its_first_delta() {
    let mut scheduler = RenderScheduler::new();
    let mut renderer = RecordingRenderer::default();
    scheduler.set_foreground(true);
    let first = full_frame(1, "A");
    offer(&mut scheduler, &first, first.clone(), 0);
    flush_frame(&mut scheduler, &mut renderer, 4, 0);

    let mut rebaseline = full_frame(1, "B");
    rebaseline.stream_id = "stream-b".to_string();
    rebaseline.grid_epoch = "epoch-b".to_string();
    let mut after_rebaseline = delta_frame(2, "C");
    after_rebaseline.stream_id = "stream-b".to_string();
    after_rebaseline.grid_epoch = "epoch-b".to_string();
    after_rebaseline.scrollback_append = vec![row_shell(0, &[])];
    after_rebaseline.scrollback_total = 1;
    let mut rebaseline_canonical = full_frame(2, "C");
    rebaseline_canonical.stream_id = "stream-b".to_string();
    rebaseline_canonical.grid_epoch = "epoch-b".to_string();
    rebaseline_canonical.scrollback_total = 1;
    rebaseline_canonical.sb_base = 1;

    let opened = offer(&mut scheduler, &rebaseline, rebaseline.clone(), 8);
    assert_eq!(
        opened.decision,
        EnqueueDecision::Coalesced {
            mode: ApplyMode::WireFull,
            batch_frames: 1,
            appended_rows: 0,
        },
        "a full on a NEW stream is a rebaseline, not a stale baseline"
    );
    offer(&mut scheduler, &after_rebaseline, rebaseline_canonical, 10);

    let repair = flush_frame(&mut scheduler, &mut renderer, 14, 0);
    assert_eq!(
        repair.mode,
        Some(ApplyMode::FallbackFull),
        "a delta may never join a queued full"
    );
    assert!(renderer.delta_seqs().is_empty());
    let delivery = repair.delivery.expect("the queued full painted");
    assert_eq!(delivery.frame.stream_id, "stream-b");
    assert_eq!(delivery.frame.seq, 2);
    assert!(
        !delivery.frame.full,
        "the delivery names the delta the queued full could not carry"
    );
    assert_eq!(delivery.canonical.stream_id, "stream-b");
    assert_eq!(delivery.canonical.grid_epoch, "epoch-b");
    assert_eq!(delivery.canonical.seq, 2);
    assert!(delivery.canonical.full);
    assert!(delivery.scrollback_appended);
    assert!(
        delivery.had_wire_full,
        "the rebaseline full is in the batch's history even though the batch applies as a fallback"
    );
    assert_eq!(
        repair.queue_delay_ms,
        Some(6),
        "a wire full restamps the queue clock, and a fallback that keeps it inherits the stamp"
    );
}

#[test]
fn repairs_a_queued_sequence_gap_after_ignoring_stale_or_conflicting_fulls() {
    let mut scheduler = RenderScheduler::new();
    let mut renderer = RecordingRenderer::default();
    scheduler.set_foreground(true);
    let baseline = full_frame(1, "A");
    offer(&mut scheduler, &baseline, baseline.clone(), 0);
    flush_frame(&mut scheduler, &mut renderer, 4, 0);

    offer(&mut scheduler, &delta_frame(2, "B"), full_frame(2, "B"), 8);

    let stale = full_frame(1, "S");
    assert_eq!(
        offer(&mut scheduler, &stale, stale.clone(), 10).decision,
        EnqueueDecision::RefusedStaleFull,
        "a full behind the queued canonical is dropped outright"
    );
    let mut conflicting = full_frame(2, "X");
    conflicting.grid_epoch = "epoch-b".to_string();
    assert_eq!(
        offer(&mut scheduler, &conflicting, conflicting.clone(), 11).decision,
        EnqueueDecision::RefusedStaleFull,
        "a full at the same sequence on a different grid is a collision, not a rebaseline"
    );
    assert_eq!(
        scheduler.pending_mode(),
        Some(ApplyMode::DeltaBatch),
        "a refused full leaves the queued batch exactly as it was"
    );

    flush_frame(&mut scheduler, &mut renderer, 12, 0);
    assert_eq!(renderer.delta_seqs(), vec![vec![2]]);

    let gapped = offer(&mut scheduler, &delta_frame(4, "D"), full_frame(4, "D"), 16);
    assert_eq!(
        gapped.decision,
        EnqueueDecision::Coalesced {
            mode: ApplyMode::FallbackFull,
            batch_frames: 1,
            appended_rows: 0,
        },
        "a delta whose base the painted DOM never reached cannot ride"
    );
    let repair = flush_frame(&mut scheduler, &mut renderer, 20, 0);
    assert_eq!(repair.mode, Some(ApplyMode::FallbackFull));
    assert_eq!(renderer.full_seqs(), vec![1, 4]);
    assert_eq!(renderer.delta_seqs(), vec![vec![2]]);
}

#[test]
fn owns_queued_delta_row_shells_before_later_replica_folding() {
    let mut scheduler = RenderScheduler::new();
    let mut renderer = RecordingRenderer::default();
    scheduler.set_foreground(true);
    let first = full_frame(1, "A");
    offer(&mut scheduler, &first, first.clone(), 0);
    flush_frame(&mut scheduler, &mut renderer, 4, 0);

    let mut second = delta_frame(2, "B");
    second.scrollback_append = vec![row_shell(0, &["A"])];
    second.scrollback_total = 1;
    offer(&mut scheduler, &second, full_frame(2, "B"), 8);

    // The replica keeps folding and renumbers the rows it handed over.
    second.viewport_rows[0].index = 99;
    second.scrollback_append[0].index = 99;

    let paint = flush_frame(&mut scheduler, &mut renderer, 12, 0);
    assert_eq!(paint.mode, Some(ApplyMode::DeltaBatch));
    let painted = &renderer.delta_batches[0][0];
    assert_eq!(
        painted.viewport_rows[0].index, 0,
        "a queued row keeps the coordinate it was queued at"
    );
    assert_eq!(
        painted.scrollback_append[0].index, 0,
        "including the history row a later fold renumbered"
    );
    assert_eq!(
        painted.viewport_rows[0].spans[0].text, "B",
        "and the cells themselves are shared, never copied to renumber a row"
    );
}

#[test]
fn parks_canonical_state_without_dom_application() {
    let mut scheduler = RenderScheduler::new();
    let mut renderer = RecordingRenderer::default();
    scheduler.set_foreground(true);
    let first = full_frame(1, "A");
    assert!(offer(&mut scheduler, &first, first.clone(), 0).armed);
    assert!(scheduler.is_frame_armed());

    scheduler.set_foreground(false);
    assert!(!scheduler.is_foreground());
    assert!(
        !scheduler.is_frame_armed(),
        "parking drops the queued frame"
    );
    assert!(!scheduler.needs_browser_frame());

    let mut latest_canonical = full_frame(2, "B");
    latest_canonical.scrollback_total = 1;
    latest_canonical.sb_base = 1;
    let mut parked = delta_frame(2, "B");
    parked.scrollback_append = vec![row_shell(0, &[])];
    parked.scrollback_total = 1;
    let queued = offer(&mut scheduler, &parked, latest_canonical, 8);
    assert_eq!(
        queued.decision,
        EnqueueDecision::Coalesced {
            mode: ApplyMode::FallbackFull,
            batch_frames: 2,
            appended_rows: 1,
        },
        "a parked pane keeps the batch it already held and counts the arrival"
    );
    assert!(!queued.armed, "a parked pane arms nothing");
    assert!(renderer.full_seqs().is_empty());
    assert!(renderer.delta_seqs().is_empty());

    scheduler.set_foreground(true);
    assert!(scheduler.is_foreground());
    assert!(scheduler.needs_browser_frame());
    assert!(scheduler.schedule_browser_frame());
    let resumed = flush_frame(&mut scheduler, &mut renderer, 20, 0);
    assert_eq!(resumed.mode, Some(ApplyMode::FallbackFull));
    assert_eq!(renderer.full_seqs(), vec![2]);
    assert!(renderer.delta_seqs().is_empty());
    let delivery = resumed
        .delivery
        .expect("the parked batch published a delivery");
    assert_eq!(delivery.frame.seq, 2);
    assert!(!delivery.frame.full);
    assert!(delivery.scrollback_appended);
    assert!(
        delivery.had_wire_full,
        "the baseline full the pane never painted is still in the batch's history"
    );
    assert_eq!(
        resumed.queue_delay_ms,
        Some(20),
        "a parked batch keeps its original queue clock"
    );
}

#[test]
fn cancels_a_queued_frame_on_disposal() {
    let mut scheduler = RenderScheduler::new();
    let renderer = RecordingRenderer::default();
    scheduler.set_foreground(true);
    let first = full_frame(1, "A");
    assert!(offer(&mut scheduler, &first, first.clone(), 0).armed);

    scheduler.dispose();
    assert!(scheduler.is_disposed());
    assert!(!scheduler.is_frame_armed());
    assert!(!scheduler.needs_browser_frame());
    assert_eq!(scheduler.on_frame_fired(12, 0), FrameDecision::Idle);
    assert!(renderer.full_seqs().is_empty());
    assert_eq!(
        offer(&mut scheduler, &first, first.clone(), 16).decision,
        EnqueueDecision::RefusedDisposed,
        "a disposed scheduler retains nothing"
    );
}
