//! The pane presentation controller on a fake clock: the foreground DOM stall
//! behind `catching_up`, the reader holds that defer it, the detached-view
//! grace, and the bounded receiving window. Ports
//! `apps/web/tests/renderer/terminalPresentation.test.ts`; `advance` fires due
//! deadlines at their own instant, as the v2 fake timers did.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod render_presentation_support;

use render_presentation_support::{
    ACCEPTED, ACCEPTED_WITHOUT_BASELINE, PaneHarness, StubRenderer, watermark,
};
use roost_web_terminal::reader_intent::ReaderIntentReason;
use roost_web_terminal::terminal_presentation::{
    DETACHED_GRACE_MS, FOREGROUND_DOM_STALL_MS, FRAME_ACTIVITY_WINDOW_MS,
    TerminalPresentationState, preserves_foreground_reader_hold,
};

#[test]
fn uses_a_one_second_foreground_dom_stall_deadline() {
    assert_eq!(FOREGROUND_DOM_STALL_MS, 1_000);
}

#[test]
fn fires_at_the_oldest_unreconciled_watermark_without_resetting_on_newer_frames() {
    let renderer = StubRenderer::at(watermark("epoch-a", 1), watermark("epoch-a", 0));
    let mut pane = PaneHarness::new(ACCEPTED, renderer);

    pane.refresh();
    assert_eq!(pane.state(), TerminalPresentationState::CatchingUp);
    pane.advance(FOREGROUND_DOM_STALL_MS / 2);
    pane.renderer_mut().canonical = watermark("epoch-a", 2);
    pane.refresh();
    pane.advance(FOREGROUND_DOM_STALL_MS / 2 - 1);
    assert_eq!(pane.stalled, vec![]);
    pane.advance(1);
    assert_eq!(pane.stalled, vec![watermark("epoch-a", 1)]);

    pane.refresh();
    pane.advance(FOREGROUND_DOM_STALL_MS);
    assert_eq!(
        pane.stalled,
        vec![watermark("epoch-a", 1), watermark("epoch-a", 2)]
    );

    pane.renderer_mut().reconciled = watermark("epoch-a", 2);
    pane.refresh();
    assert_eq!(pane.state(), TerminalPresentationState::Idle);
}

#[test]
fn inactive_panes_cancel_a_pending_catch_up_callback() {
    let renderer = StubRenderer::at(watermark("epoch-a", 2), watermark("epoch-a", 1));
    let mut pane = PaneHarness::new(ACCEPTED, renderer);

    pane.refresh();
    pane.pane.active = false;
    pane.refresh();
    pane.advance(FOREGROUND_DOM_STALL_MS);
    assert!(pane.stalled.is_empty());
    assert_eq!(pane.state(), TerminalPresentationState::Idle);
}

#[test]
fn a_long_lived_selection_defers_recovery_without_consuming_the_same_generation_stall() {
    let mut renderer = StubRenderer::at(watermark("epoch-a", 2), watermark("epoch-a", 1));
    renderer.reader_reason = Some(ReaderIntentReason::Selection);
    let mut pane = PaneHarness::new(ACCEPTED, renderer);

    pane.refresh();
    pane.advance(FOREGROUND_DOM_STALL_MS * 2);
    assert!(pane.stalled.is_empty());
    assert_eq!(pane.state(), TerminalPresentationState::CatchingUp);

    pane.renderer_mut().reader_reason = None;
    pane.refresh();
    pane.advance(FOREGROUND_DOM_STALL_MS - 1);
    assert!(pane.stalled.is_empty());
    pane.advance(1);
    assert_eq!(pane.stalled.len(), 1);
}

#[test]
fn preserves_every_explicit_reader_hold() {
    assert!(preserves_foreground_reader_hold(Some(ReaderIntentReason::NativeScroll)));
    assert!(preserves_foreground_reader_hold(Some(ReaderIntentReason::Wheel)));
    assert!(preserves_foreground_reader_hold(Some(ReaderIntentReason::Touch)));
    assert!(preserves_foreground_reader_hold(Some(ReaderIntentReason::Selection)));
    assert!(preserves_foreground_reader_hold(Some(ReaderIntentReason::Find)));
    assert!(!preserves_foreground_reader_hold(None));
}

#[test]
fn an_actively_viewed_pane_without_an_accepted_view_detaches_once_the_grace_expires() {
    let mut pane = PaneHarness::new(ACCEPTED_WITHOUT_BASELINE, StubRenderer::reconciled());

    pane.refresh();
    assert_eq!(pane.state(), TerminalPresentationState::Idle);
    pane.advance(DETACHED_GRACE_MS - 1);
    assert_eq!(pane.state(), TerminalPresentationState::Idle);
    pane.advance(1);
    assert_eq!(pane.state(), TerminalPresentationState::Detached);
}

#[test]
fn the_grace_runs_from_the_first_lost_view_not_from_the_latest_refresh() {
    let mut pane = PaneHarness::new(ACCEPTED_WITHOUT_BASELINE, StubRenderer::reconciled());

    pane.refresh();
    for _ in 0..DETACHED_GRACE_MS / 100 {
        pane.advance(100);
        pane.refresh();
    }
    assert_eq!(pane.state(), TerminalPresentationState::Detached);
}

#[test]
fn a_view_that_becomes_ready_inside_the_grace_never_detaches() {
    let mut pane = PaneHarness::new(ACCEPTED_WITHOUT_BASELINE, StubRenderer::reconciled());

    pane.refresh();
    pane.advance(DETACHED_GRACE_MS - 1);
    pane.status = Some(ACCEPTED);
    pane.refresh();
    assert_eq!(pane.state(), TerminalPresentationState::Idle);
    pane.advance(DETACHED_GRACE_MS * 3);
    assert_eq!(pane.state(), TerminalPresentationState::Idle);
}

#[test]
fn panes_that_are_not_actively_viewed_never_detach() {
    let mut inactive = PaneHarness::new(ACCEPTED_WITHOUT_BASELINE, StubRenderer::reconciled());
    inactive.pane.active = false;
    let mut hidden = PaneHarness::new(ACCEPTED_WITHOUT_BASELINE, StubRenderer::reconciled());
    hidden.pane.page_visible = false;

    inactive.refresh();
    hidden.refresh();
    inactive.advance(DETACHED_GRACE_MS * 10);
    hidden.advance(DETACHED_GRACE_MS * 10);
    assert_eq!(inactive.state(), TerminalPresentationState::Idle);
    assert_eq!(hidden.state(), TerminalPresentationState::Idle);
}

#[test]
fn a_detached_pane_reports_receiving_again_on_the_next_accepted_delta() {
    let mut pane = PaneHarness::new(ACCEPTED_WITHOUT_BASELINE, StubRenderer::reconciled());

    pane.refresh();
    pane.advance(DETACHED_GRACE_MS);
    assert_eq!(pane.state(), TerminalPresentationState::Detached);
    pane.status = Some(ACCEPTED);
    pane.note_frame(false, "epoch-a", 5);
    assert_eq!(pane.state(), TerminalPresentationState::Receiving);
}

/// The receiving dot is bounded: with no further frame, its own deadline
/// carries the pane back to idle at the window edge.
#[test]
fn receiving_expires_to_idle_at_the_window_edge_without_another_frame() {
    let mut pane = PaneHarness::new(ACCEPTED, StubRenderer::reconciled());

    pane.note_frame(false, "epoch-a", 5);
    assert_eq!(pane.state(), TerminalPresentationState::Receiving);
    pane.advance(FRAME_ACTIVITY_WINDOW_MS - 1);
    assert_eq!(pane.state(), TerminalPresentationState::Receiving);
    pane.advance(1);
    assert_eq!(pane.state(), TerminalPresentationState::Idle);
    assert_eq!(pane.controller.next_deadline_ms(), None);
}

/// A full is a repair or an attach, not output: it never lights the dot.
#[test]
fn a_full_frame_is_not_activity() {
    let mut pane = PaneHarness::new(ACCEPTED, StubRenderer::reconciled());

    assert_eq!(pane.note_frame(true, "epoch-a", 5), TerminalPresentationState::Idle);
}

#[test]
fn the_cursor_blinks_only_on_the_focused_viewed_pane_of_a_visible_page() {
    let mut pane = PaneHarness::new(ACCEPTED, StubRenderer::reconciled());
    let blink = |pane: &mut PaneHarness| {
        let facts = pane.pane;
        pane.controller.refresh_cursor_blink(facts, pane.renderer.as_mut());
        pane.renderer_mut().cursor_blink
    };

    assert_eq!(blink(&mut pane), Some(true));
    pane.pane.focused = false;
    assert_eq!(blink(&mut pane), Some(false));
    pane.pane.focused = true;
    pane.pane.page_visible = false;
    assert_eq!(blink(&mut pane), Some(false));
    pane.pane.page_visible = true;
    pane.pane.active = false;
    assert_eq!(blink(&mut pane), Some(false));
    pane.pane.active = true;
    assert_eq!(blink(&mut pane), Some(true));
    pane.controller.dispose(pane.renderer.as_mut());
    assert_eq!(pane.renderer_mut().cursor_blink, Some(false));
}
