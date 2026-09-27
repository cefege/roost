//! The selection guard: which selections hold paint, which restore, and which
//! end their capture. Every case is a way a pane freezes or a stale range
//! comes back, so each one is decided natively with the DOM replaced by the
//! two facts an adapter reads — the live selection and the retained range.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_web_terminal::input::{
    ComposeSelection, ComposerSelection, DomNodeId, FocusOwner, LiveSelection, OwnedRow,
    PaneInputs, RetainedRange, SelectionDirection, SelectionEndpoint, SelectionGuard, YieldLapse,
};

mod selection_guard_support;

use selection_guard_support::*;

#[test]
fn a_selection_dropped_while_the_panes_listeners_are_detached_stops_holding_paint() {
    let mut guard = SelectionGuard::new();
    let live = pane_selection("v0");
    assert!(guard.sync_hold(&live, None).hold);
    // The withdraw: the pane's global listeners are gone, so nothing
    // re-derives the hold for the whole of the park.
    guard.release_paint_holds();
    assert!(!guard.sync_hold(&no_selection(), None).hold);
    // The re-attach re-derives from the live document, so a selection the user
    // never dropped keeps holding.
    assert!(guard.sync_hold(&live, None).hold);
}

#[test]
fn a_selection_still_live_when_the_listeners_re_attach_keeps_paint_held() {
    let mut guard = SelectionGuard::new();
    let live = pane_selection("v0");
    assert!(guard.sync_hold(&live, None).hold);
    guard.release_paint_holds();
    let reattached = guard.sync_hold(&live, None);
    assert!(reattached.hold && reattached.lapse.is_none());
}

#[test]
fn a_selection_anchored_in_the_terminal_is_held_against_a_select_all_the_renderer_did_not_ask_for()
{
    let mut guard = SelectionGuard::new();
    // The user dragged across the terminal: a live pane-owned range.
    assert!(guard.sync_hold(&pane_selection("v"), None).hold);
    // A programmatic select-all of the same display lands a LARGER, different
    // range. No renderer interaction followed it, so nothing released the hold.
    let after = guard.sync_hold(&pane_selection("v0 more rows"), None);
    assert!(
        after.hold,
        "a select-all the renderer did not ask for is still the user's selection"
    );
    assert_eq!(after.lapse, None);
    // A select-all of a DIFFERENT element is not this pane's.
    assert!(!guard.sync_hold(&foreign_selection(), None).hold);
}

#[test]
fn a_suspension_whose_restore_never_runs_stops_holding_paint_once_its_range_is_gone() {
    let (mut guard, _composer) = suspended_composer_pane();
    assert!(
        guard
            .sync_hold(&pane_selection("v0"), Some(&retained("v0")))
            .hold
    );
    // What a canonical repair does to the captured row: the same text painted
    // on new nodes, which the old endpoints no longer resolve to. Nothing
    // restores or releases the capture afterwards.
    let repainted = OwnedRow {
        id: DomNodeId(77),
        text: "v0".to_string(),
    };
    let repaired = retained_for("v0", vec![repainted]);
    let repaired_live = LiveSelection {
        owned_rows: Vec::new(),
        ..pane_selection("v0")
    };
    let after = guard.sync_hold(&repaired_live, Some(&repaired));
    assert!(
        !after.hold,
        "a wedge the user cannot clear by selecting nothing is the defect"
    );
    assert_eq!(after.lapse.map(YieldLapse::reason), Some("capture_gone"));
}

#[test]
fn a_suspension_whose_range_is_still_live_and_restorable_keeps_paint_held() {
    let (mut guard, _composer) = suspended_composer_pane();
    // The document's own range is GONE — the yield cleared it — while the
    // retained range is intact. The hold reads the capture, not the document,
    // or it lapses on the very clear that made the composer editable.
    let after = guard.sync_hold(&cleared_by_yield(), Some(&retained("v0")));
    assert!(after.hold);
    assert_eq!(after.lapse, None);
}

#[test]
fn a_suspension_whose_focus_owner_lost_focus_stops_holding_paint() {
    let (mut guard, _composer) = suspended_composer_pane();
    // The composer was torn down: its textarea is disconnected and the page's
    // editing target is gone with it.
    let orphaned = LiveSelection {
        focus_owner: Some(FocusOwner {
            node: COMPOSER,
            connected: false,
        }),
        ..pane_selection("v0")
    };
    let after = guard.sync_hold(&orphaned, Some(&retained("v0")));
    assert!(!after.hold);
    assert_eq!(after.lapse, Some(YieldLapse::OwnerGone));
}

#[test]
fn a_suspension_taken_while_nothing_is_focused_never_holds_paint() {
    let mut guard = SelectionGuard::new();
    let live = pane_selection("v0");
    let held = retained("v0");
    assert!(guard.capture(&live, DISPLAY));
    // The yield still happens so no keystroke is swallowed, but with no focus
    // owner there is nothing to wait for.
    assert!(guard.suspend(&live, Some(&held)));
    let after = guard.sync_hold(&no_selection(), Some(&held));
    assert!(!after.hold);
    assert_eq!(after.lapse, None);
}

#[test]
fn a_capture_is_refused_for_a_collapsed_or_foreign_selection() {
    let mut guard = SelectionGuard::new();
    assert!(!guard.capture(&caret(the_row()), DISPLAY));
    assert!(!guard.capture(&foreign_selection(), DISPLAY));
    assert!(!guard.capture(&no_selection(), DISPLAY));
    assert!(!guard.has_capture());
}

#[test]
fn a_transition_invalidates_every_capture_at_once() {
    let mut guard = SelectionGuard::new();
    let live = pane_selection("v0");
    let held = retained("v0");
    assert!(guard.capture(&live, DISPLAY));
    let before = guard.epoch();
    guard.prepare_live_interaction();
    assert!(guard.epoch() > before && !guard.has_capture());
    assert!(!guard.restore(&live, Some(&held)));
}

#[test]
fn a_capture_scoped_to_one_display_does_not_restore_into_another() {
    let mut guard = SelectionGuard::new();
    let live = pane_selection("v0");
    let mut held = retained("v0");
    assert!(guard.capture(&live, DISPLAY));
    held.display = DomNodeId(55);
    assert!(
        !guard.restore(&live, Some(&held)),
        "a remount replaced the display, and the old range is not this pane's"
    );
}

#[test]
fn a_capture_is_refused_when_another_owner_established_a_range_in_its_place() {
    let mut guard = SelectionGuard::new();
    let live = pane_selection("v0");
    let held = retained("v0");
    assert!(guard.capture(&live, DISPLAY));
    // A non-collapsed selection elsewhere is another owner, not the browser's
    // editable-focus artifact, so the capture must not clear or replace it.
    let sidebar = OwnedRow {
        id: DomNodeId(88),
        text: "sidebar".to_string(),
    };
    let elsewhere = LiveSelection {
        owned_rows: vec![sidebar],
        ..foreign_selection()
    };
    assert!(!guard.restore(&elsewhere, Some(&held)));
}

#[test]
fn the_composer_suspends_the_pane_range_and_restores_it_after_its_edit() {
    let mut guard = SelectionGuard::new();
    let (mut composer, live, held) = capture_v0(&mut guard);
    composer.remember_caret_at(4);
    let effects = composer.on_key_or_before_input(&mut guard, inputs(&live, &held));
    let remembered = ComposerSelection::caret(4);
    assert_eq!(effects.set_composer_selection, Some(remembered));
    assert!(composer.composer_selection_active());
    // The default edit ran; the layout transaction restores after it.
    assert!(composer.on_key_up(7).schedule_layout_restore.is_some());
    let (version, epoch) = composer.pending_layout_version().expect("armed");
    assert!(composer.layout_restore_is_current(version, epoch));
    composer.finish_layout_restore();
    assert!(composer.restore(&mut guard, inputs(&live, &held)).restore);
    assert!(!composer.composer_selection_active());
}

#[test]
fn a_superseded_layout_restore_cannot_restore_over_a_newer_capture() {
    let mut guard = SelectionGuard::new();
    let (mut composer, _live, held) = capture_v0(&mut guard);
    composer.on_key_up(3);
    let (stale_version, stale_epoch) = composer.pending_layout_version().expect("armed");
    let other = pane_selection("other");
    composer.capture(&mut guard, inputs(&other, &held));
    // A stale timer must be inert, or a stale caret lands over a newer range.
    assert!(!composer.layout_restore_is_current(stale_version, stale_epoch));
}

#[test]
fn the_clear_a_yield_performs_is_not_read_as_the_user_dropping_the_selection() {
    let mut guard = SelectionGuard::new();
    let (mut composer, live, held) = capture_v0(&mut guard);
    assert!(composer.suspend(&mut guard, inputs(&live, &held)).is_some());
    // The document reports the empty selection the yield's own clear produced.
    let effects = composer.on_document_selection_change(&no_selection(), true);
    assert!(
        !effects.release,
        "releasing here would drop the capture the composer is still editing behind"
    );
    assert!(composer.has_guard());
    // A collapse that is NOT the yield's clear, focus outside the dock, is a
    // genuine abandonment.
    let outside = composer.on_document_selection_change(&no_selection(), false);
    assert!(outside.release);
}

#[test]
fn a_programmatic_write_captures_and_suspends_without_a_pointerdown() {
    let mut guard = SelectionGuard::new();
    let mut composer = ComposeSelection::new();
    let live = focused_by_composer(&pane_selection("v0"));
    let held = retained("v0");
    composer.prepare_programmatic_write(&mut guard, inputs(&live, &held));
    assert!(composer.has_guard());
    assert!(guard.sync_hold(&live, Some(&held)).hold);
    composer.release(&mut guard);
    assert!(!composer.has_guard());
    assert!(!guard.has_capture());
}

#[test]
fn the_composer_remembers_its_own_selection_direction() {
    let mut composer = ComposeSelection::new();
    composer.remember_composer_selection(ComposerSelection {
        start: 2,
        end: 7,
        direction: SelectionDirection::Backward,
    });
    let remembered = composer.remembered_selection();
    assert_eq!((remembered.start, remembered.end), (2, 7));
    assert_eq!(remembered.direction, SelectionDirection::Backward);

    composer.remember_caret_at(0);
    assert_eq!(
        composer.remembered_selection().direction,
        SelectionDirection::None,
        "a bare caret has no direction to restore"
    );
}

#[test]
fn an_ime_composition_holds_the_composer_caret_and_arms_no_browser_edit() {
    let mut guard = SelectionGuard::new();
    let (mut composer, live, held) = capture_v0(&mut guard);
    composer.on_composition_start(&mut guard, inputs(&live, &held));
    assert!(composer.is_composing());
    assert!(
        composer
            .on_key_or_before_input(&mut guard, inputs(&live, &held))
            .set_composer_selection
            .is_none(),
        "the composer already owns the caret, and must not be given it twice"
    );
    composer.on_composition_end();
    assert!(!composer.is_composing());
    assert!(composer.on_key_up(9).schedule_layout_restore.is_some());
}
