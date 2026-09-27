//! The reader state machine: parking, holding, and the one question that keeps
//! a stalled pane stalled — whether a park may be left.
//!
//! Every one of these transitions is a way a reader gets frozen for good. A
//! hold armed on an edge that never clears; a `find` park that outlives the
//! find bar; a wheel park created after its gesture's last scroll event, with
//! nothing left to resume it. They are all decidable without a browser, so they
//! are decided here rather than in a screenshot.

use roost_web_terminal::reader_intent::{
    BOTTOM_FOLLOW_SLACK_ROWS, EnterReadingOutcome, HoldChange, RENDERER_HOLD_LINK,
    RENDERER_HOLD_SELECTION, ReaderAnchor, ReaderIntent, ReaderIntentReason, ReaderState,
    ReconcileBlockReason, ScrollBoxGeometry, follows_scroll_bottom, reader_anchor_at_scroll,
};

fn live() -> ReaderState {
    ReaderState::new()
}

#[test]
fn a_fresh_renderer_is_live_unheld_and_unparked() {
    let state = live();
    assert_eq!(state.intent(), ReaderIntent::Live);
    assert_eq!(state.reason(), None);
    assert_eq!(state.hold_mask(), 0);
    assert!(!state.holding());
}

#[test]
fn a_native_scroll_parks_the_reader_and_names_its_own_reason() {
    let mut state = live();
    assert_eq!(
        state.enter_reading(ReaderIntentReason::NativeScroll),
        EnterReadingOutcome::Parked
    );
    assert_eq!(state.intent(), ReaderIntent::Reading);
    assert_eq!(state.reason(), Some(ReaderIntentReason::NativeScroll));
}

#[test]
fn a_selection_that_starts_inside_a_find_park_keeps_the_find_reason() {
    let mut state = live();
    state.enter_reading(ReaderIntentReason::Find);
    assert_eq!(
        state.enter_reading(ReaderIntentReason::Selection),
        EnterReadingOutcome::AnchorOnly
    );
    assert_eq!(
        state.reason(),
        Some(ReaderIntentReason::Find),
        "the anchor-owning park outlives the selection that started on it"
    );
    state.end_find_reading();
    assert_eq!(state.reason(), Some(ReaderIntentReason::NativeScroll));
}

#[test]
fn dismissing_a_find_downgrades_the_park_without_moving_it() {
    let mut state = live();
    state.enter_reading(ReaderIntentReason::Find);
    state.end_find_reading();
    assert_eq!(state.intent(), ReaderIntent::Reading);
    assert_eq!(state.reason(), Some(ReaderIntentReason::NativeScroll));
    assert!(
        state
            .reason()
            .is_some_and(ReaderIntentReason::is_position_only),
        "a dismissed find park is an ordinary scroll park any resume can release"
    );
}

#[test]
fn ending_a_find_interval_that_never_existed_changes_nothing() {
    let mut state = live();
    state.enter_reading(ReaderIntentReason::Wheel);
    state.end_find_reading();
    assert_eq!(state.reason(), Some(ReaderIntentReason::Wheel));
}

#[test]
fn the_selection_hold_parks_the_reader_and_the_link_hold_does_not() {
    let mut selection = live();
    assert_eq!(selection.set_selection_hold(true), HoldChange::Armed);
    assert_eq!(selection.intent(), ReaderIntent::Reading);
    assert_eq!(selection.reason(), Some(ReaderIntentReason::Selection));
    assert_eq!(selection.hold_mask(), RENDERER_HOLD_SELECTION);

    let mut link = live();
    assert_eq!(link.set_armed_hold(true), HoldChange::Armed);
    assert_eq!(link.intent(), ReaderIntent::Live);
    assert_eq!(link.reason(), None);
    assert_eq!(link.hold_mask(), RENDERER_HOLD_LINK);
}

#[test]
fn re_requesting_a_hold_that_is_already_set_reports_no_change() {
    let mut state = live();
    state.set_selection_hold(true);
    assert_eq!(state.set_selection_hold(true), HoldChange::Unchanged);
    assert_eq!(state.hold_mask(), RENDERER_HOLD_SELECTION);
}

#[test]
fn a_hold_release_is_offered_only_where_a_resume_could_actually_end_the_park() {
    // A selection park is ended by its own release.
    let mut selection = live();
    selection.set_selection_hold(true);
    selection.set_selection_hold(false);
    assert!(selection.should_flush_after_release(false, false));

    // A find park keeps its interval while it can still reach its anchor.
    let mut find = live();
    find.enter_reading(ReaderIntentReason::Find);
    assert!(!find.should_flush_after_release(false, false));
    // ... and yields once the box has no scroll range left to reach it with.
    assert!(find.should_flush_after_release(true, false));

    // A band-following position-only park has already come home; the hold
    // swallowed the scroll event that proved it.
    let mut wheel = live();
    wheel.enter_reading(ReaderIntentReason::Wheel);
    assert!(!wheel.should_flush_after_release(false, false));
    assert!(wheel.should_flush_after_release(false, true));

    // A position-only park with range left keeps its interval: a real wheel
    // still recovers it.
    let mut native = live();
    native.enter_reading(ReaderIntentReason::NativeScroll);
    assert!(!native.should_flush_after_release(false, false));
}

#[test]
fn a_surviving_hold_outranks_a_resume_and_leaves_the_reader_state_truthful() {
    let mut state = live();
    state.set_armed_hold(true);
    // A local keystroke is v2's `prepareLiveInteraction`: it clears the holds
    // AND is explicit, so it pins. `clear_holds` alone never pins — v2 has no
    // callsite that clears holds without also being explicit.
    let admission = state.begin_resume(true, true);
    assert!(
        admission.admitted,
        "an explicit local interaction clears the holds"
    );
    assert!(admission.pin_on_resume);

    let mut both = live();
    both.set_selection_hold(true);
    both.set_armed_hold(true);
    let refused = both.begin_resume(false, false);
    assert!(!refused.admitted);
    assert_eq!(
        both.hold_mask(),
        RENDERER_HOLD_SELECTION | RENDERER_HOLD_LINK
    );
    assert_eq!(
        both.intent(),
        ReaderIntent::Reading,
        "a held pane keeps reporting its real reason, so a watchdog stays muted"
    );
}

#[test]
fn an_implicit_resume_never_touches_a_find_park_but_an_explicit_one_does() {
    let mut state = live();
    state.enter_reading(ReaderIntentReason::Find);
    assert!(!state.begin_resume(false, false).admitted);
    assert_eq!(state.intent(), ReaderIntent::Reading);
    let explicit = state.begin_resume(false, true);
    assert!(explicit.admitted);
    assert!(explicit.pin_on_resume);
    assert_eq!(state.intent(), ReaderIntent::Live);
    assert_eq!(state.reason(), None);
}

#[test]
fn a_selection_park_resumes_with_a_pin_because_that_is_what_it_asked_for() {
    let mut state = live();
    state.set_selection_hold(true);
    state.set_selection_hold(false);
    let admission = state.begin_resume(false, false);
    assert!(admission.admitted);
    assert!(
        admission.pin_on_resume,
        "a reader who selected to read should come back to the live tail"
    );
}

#[test]
fn only_the_three_gesture_reasons_are_position_only() {
    assert!(ReaderIntentReason::NativeScroll.is_position_only());
    assert!(ReaderIntentReason::Wheel.is_position_only());
    assert!(ReaderIntentReason::Touch.is_position_only());
    assert!(!ReaderIntentReason::Selection.is_position_only());
    assert!(!ReaderIntentReason::Find.is_position_only());
}

#[test]
fn the_follow_band_absorbs_sub_row_jitter_and_still_lets_a_wheel_notch_out() {
    let row = 16.8;
    let band = f64::from(BOTTOM_FOLLOW_SLACK_ROWS) * row;
    let at_clamp = ScrollBoxGeometry {
        scroll_top: 1000.0,
        scroll_height: 1400.0,
        client_height: 400.0,
    };
    assert!(follows_scroll_bottom(at_clamp, row));
    let one_row_short = ScrollBoxGeometry {
        scroll_top: at_clamp.scroll_top - 1.0,
        ..at_clamp
    };
    assert!(follows_scroll_bottom(one_row_short, row));
    let past_the_band = ScrollBoxGeometry {
        scroll_top: at_clamp.scroll_top - band - 1.0,
        ..at_clamp
    };
    assert!(!follows_scroll_bottom(past_the_band, row));
    let a_wheel_notch = ScrollBoxGeometry {
        scroll_top: at_clamp.scroll_top - 100.0,
        ..at_clamp
    };
    assert!(
        !follows_scroll_bottom(a_wheel_notch, row),
        "one wheel notch leaves the band, which is what parks the pane"
    );
}

#[test]
fn an_unmeasured_pane_evaluates_the_band_against_the_default_pitch() {
    let at_clamp = ScrollBoxGeometry {
        scroll_top: 0.0,
        scroll_height: 1000.0,
        client_height: 1000.0,
    };
    assert!(follows_scroll_bottom(at_clamp, 0.0));
    assert!(follows_scroll_bottom(at_clamp, -5.0));
}

#[test]
fn a_reader_anchor_is_the_row_its_scroll_position_lands_in() {
    let anchor: ReaderAnchor = reader_anchor_at_scroll(1000.0, 40.0, 20.0, 500).expect("inside");
    assert_eq!(anchor.row, 48);
    assert_eq!(anchor.offset_px, 0.0);
    let partial = reader_anchor_at_scroll(1050.0, 40.0, 20.0, 500).expect("inside");
    assert_eq!(partial.row, 50);
    assert_eq!(partial.offset_px, 10.0);
}

#[test]
fn a_position_at_or_past_the_end_of_layout_has_no_anchor_to_come_back_to() {
    assert!(reader_anchor_at_scroll(1000.0, 0.0, 20.0, 50).is_none());
    assert!(reader_anchor_at_scroll(1000.0, 0.0, 20.0, 0).is_none());
    assert!(reader_anchor_at_scroll(1000.0, 0.0, 0.0, 500).is_none());
}

#[test]
fn the_reconcile_block_reason_names_the_thing_that_is_actually_blocking() {
    let mut state = live();
    let reconciled = (Some("epoch-1"), Some(7u64));
    assert_eq!(
        state.reconcile_block_reason(false, false, reconciled, reconciled),
        ReconcileBlockReason::None
    );
    assert_eq!(
        state.reconcile_block_reason(true, false, reconciled, reconciled),
        ReconcileBlockReason::ReaderPendingFrame
    );
    assert_eq!(
        state.reconcile_block_reason(false, true, reconciled, reconciled),
        ReconcileBlockReason::PendingRender
    );
    assert_eq!(
        state.reconcile_block_reason(false, false, (Some("epoch-1"), Some(8)), reconciled),
        ReconcileBlockReason::NotReconciled
    );

    state.set_selection_hold(true);
    assert_eq!(
        state.reconcile_block_reason(false, false, reconciled, reconciled),
        ReconcileBlockReason::SelectionHold
    );
    state.set_armed_hold(true);
    assert_eq!(
        state.reconcile_block_reason(false, false, reconciled, reconciled),
        ReconcileBlockReason::SelectionAndLinkHold
    );

    let mut link_only = live();
    link_only.set_armed_hold(true);
    assert_eq!(
        link_only.reconcile_block_reason(false, false, reconciled, reconciled),
        ReconcileBlockReason::LinkHold
    );
}

#[test]
fn a_reader_pending_frame_outranks_every_hold_in_the_block_reason() {
    let mut state = live();
    state.set_armed_hold(true);
    assert_eq!(
        state.reconcile_block_reason(true, true, (Some("a"), Some(1)), (Some("a"), Some(1))),
        ReconcileBlockReason::ReaderPendingFrame
    );
}
