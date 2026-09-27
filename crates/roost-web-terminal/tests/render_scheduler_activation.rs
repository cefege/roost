//! A view's whole life against the scheduler: parked deliveries folding
//! canonically, a renderer-only dropped delta repaired by the next
//! checkpoint, and a stream reset cancelling a queued frame. Ported from
//! `apps/web/tests/terminalStreamRenderScheduler.test.ts`.
//!
//! The replica and the socket those cases drive are the caller's, so each one
//! here is the scheduler's own question: what is retained while nothing paints,
//! what repairs a sequence the DOM never reached, and what a reset must leave
//! behind.

mod render_scheduler_support;

use render_scheduler_support::{
    RecordingRenderer, delta_frame, flush_frame, full_frame, offer, row_shell,
};
use roost_web_terminal::scheduler::{ApplyMode, FrameDecision, RenderScheduler};

#[test]
fn folds_parked_deliveries_canonically_and_repairs_with_the_latest_full_on_activation() {
    let mut scheduler = RenderScheduler::new();
    let mut renderer = RecordingRenderer::default();
    scheduler.set_foreground(true);
    scheduler.set_foreground(false);

    let baseline = full_frame(1, "A");
    let queued = offer(&mut scheduler, &baseline, baseline.clone(), 0);
    assert!(!queued.armed);
    offer(&mut scheduler, &delta_frame(2, "B"), full_frame(2, "B"), 4);
    assert!(!scheduler.needs_browser_frame());
    assert!(!scheduler.schedule_browser_frame(), "a parked pane has no frame to arm");
    assert!(renderer.full_seqs().is_empty());
    assert!(renderer.delta_seqs().is_empty());

    scheduler.set_foreground(true);
    assert!(scheduler.schedule_browser_frame());
    let activation = flush_frame(&mut scheduler, &mut renderer, 20, 0);
    assert_eq!(activation.mode, Some(ApplyMode::FallbackFull));
    assert_eq!(renderer.full_seqs(), vec![2], "one paint, the newest canonical");
    let painted = &renderer.full_frames[0];
    assert!(painted.full);
    assert_eq!(painted.base_seq, 0);
    assert_eq!(painted.seq, 2);
    assert_eq!(painted.viewport_rows[0].spans[0].text, "B");
    assert!(renderer.delta_seqs().is_empty());
    assert_eq!(
        activation.delivery.expect("one delivery").frame.seq,
        2,
        "and exactly one delivery, for the delta the full replaced"
    );
}

#[test]
fn repairs_a_renderer_only_dropped_delta_with_a_viewport_only_checkpoint() {
    let mut scheduler = RenderScheduler::new();
    let mut renderer = RecordingRenderer::default();
    scheduler.set_foreground(true);
    let baseline = full_frame(1, "A");
    offer(&mut scheduler, &baseline, baseline.clone(), 0);
    flush_frame(&mut scheduler, &mut renderer, 4, 0);

    // The delta is dropped for the RENDERER alone: the replica folds it, the
    // scheduler is never offered it, and the painted DOM never reaches its seq.
    let dropped = delta_frame(2, "B");
    assert_eq!(dropped.base_seq, 1, "it extended the baseline the DOM holds");
    assert_eq!(
        scheduler.reconciled_watermark().and_then(|mark| mark.seq),
        Some(1),
        "and it never became the watermark"
    );

    let mut following = delta_frame(3, "C");
    following.scrollback_append = vec![row_shell(1, &["B"])];
    following.scrollback_total = 2;
    let mut checkpoint = full_frame(3, "C");
    checkpoint.scrollback_total = 2;
    checkpoint.sb_base = 2;
    let queued = offer(&mut scheduler, &following, checkpoint, 12);
    assert_eq!(
        queued.decision,
        EnqueueDecision::Coalesced {
            mode: ApplyMode::FallbackFull,
            batch_frames: 1,
            appended_rows: 1,
        },
        "a delta that skips a sequence repairs from the canonical checkpoint"
    );

    let repair = flush_frame(&mut scheduler, &mut renderer, 16, 0);
    assert_eq!(repair.mode, Some(ApplyMode::FallbackFull));
    let painted = renderer.full_frames.last().expect("the repair painted a full");
    assert!(painted.full);
    assert_eq!(painted.base_seq, 0);
    assert_eq!(painted.seq, 3);
    assert_eq!(painted.sb_base, checkpoint.sb_base);
    let delivery = repair.delivery.expect("the repair published a delivery");
    assert_eq!(delivery.frame.seq, 3);
    assert!(
        !delivery.frame.full,
        "the delivery names the delta; the DOM gets the canonical checkpoint"
    );
    assert_eq!(delivery.canonical.seq, 3);
    assert!(delivery.canonical.full);
    assert!(
        delivery.scrollback_appended,
        "the checkpoint still counts the row the dropped delta appended"
    );
    assert!(
        !delivery.had_wire_full,
        "nothing on the wire carried a full for this batch"
    );
}

#[test]
fn cancels_a_queued_renderer_frame_when_stream_state_resets() {
    let mut scheduler = RenderScheduler::new();
    let mut renderer = RecordingRenderer::default();
    scheduler.set_foreground(true);
    offer(&mut scheduler, &full_frame(1, "A"), full_frame(1, "A"), 0);
    assert!(scheduler.is_frame_armed());

    // The stream state resets and drops the renderer it owned.
    scheduler.dispose();
    assert!(
        !scheduler.is_frame_armed(),
        "a reset must leave no frame that can paint a dead stream"
    );
    assert!(!scheduler.needs_browser_frame());
    assert_eq!(scheduler.on_frame_fired(8, 0), FrameDecision::Idle);
    assert!(renderer.full_seqs().is_empty());

    // A replacement view starts with nothing, so its first frame must be
    // APPLIED: there is no baseline for a delta to continue from.
    let mut replacement = RenderScheduler::new();
    replacement.set_foreground(true);
    let mut replacement_renderer = RecordingRenderer::default();
    let rebaseline = full_frame(1, "A");
    offer(&mut replacement, &rebaseline, rebaseline.clone(), 12);
    let fresh = flush_frame(&mut replacement, &mut replacement_renderer, 16, 0);
    assert_eq!(fresh.mode, Some(ApplyMode::WireFull));
    assert_eq!(replacement_renderer.full_seqs(), vec![1]);
    assert_eq!(
        replacement.reconciled_watermark().and_then(|mark| mark.seq),
        Some(1)
    );
}
