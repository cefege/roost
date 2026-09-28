//! `derive_terminal_presentation_state` is the pure decision behind a pane's
//! status dot. These cases pin the activity-window boundaries, the
//! catching-up case and the unready cases, with no controller or clock.
//! Ports `apps/web/tests/terminalStreamPresentationState.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod render_presentation_support;

use render_presentation_support::watermark;
use roost_web_terminal::terminal_presentation::{
    DETACHED_GRACE_MS, FRAME_ACTIVITY_WINDOW_MS, TerminalPresentationActivity,
    TerminalPresentationInput, TerminalPresentationState, derive_terminal_presentation_state,
};

fn activity(seq: u64, started_at_ms: u64) -> TerminalPresentationActivity {
    TerminalPresentationActivity {
        grid_epoch: "epoch-a".to_owned(),
        seq,
        started_at_ms,
    }
}

#[test]
fn reports_receiving_for_recent_equal_canonical_and_reconciled_watermarks_then_idles() {
    let activity = activity(2, 1_000);
    let canonical = watermark("epoch-a", 2);
    let reconciled = watermark("epoch-a", 2);
    let at = |now_ms| {
        derive_terminal_presentation_state(TerminalPresentationInput {
            active: true,
            accepted_with_baseline: true,
            canonical: &canonical,
            reconciled: &reconciled,
            activity: Some(&activity),
            now_ms,
            not_ready_since_ms: None,
        })
    };

    assert_eq!(FRAME_ACTIVITY_WINDOW_MS, 500);
    assert_eq!(at(1_499), TerminalPresentationState::Receiving);
    assert_eq!(at(1_500), TerminalPresentationState::Idle);
}

#[test]
fn reports_catching_up_while_canonical_is_ahead_of_the_renderer() {
    let activity = activity(3, 1_000);
    let state = derive_terminal_presentation_state(TerminalPresentationInput {
        active: true,
        accepted_with_baseline: true,
        canonical: &watermark("epoch-a", 3),
        reconciled: &watermark("epoch-a", 2),
        activity: Some(&activity),
        now_ms: 1_100,
        not_ready_since_ms: None,
    });

    assert_eq!(state, TerminalPresentationState::CatchingUp);
}

#[test]
fn returns_to_receiving_after_hold_reconciliation_then_expires_to_idle() {
    let activity = activity(4, 2_000);
    let canonical = watermark("epoch-a", 4);
    let reconciled = watermark("epoch-a", 4);
    let at = |now_ms| {
        derive_terminal_presentation_state(TerminalPresentationInput {
            active: true,
            accepted_with_baseline: true,
            canonical: &canonical,
            reconciled: &reconciled,
            activity: Some(&activity),
            now_ms,
            not_ready_since_ms: None,
        })
    };

    assert_eq!(at(2_250), TerminalPresentationState::Receiving);
    assert_eq!(at(2_500), TerminalPresentationState::Idle);
}

#[test]
fn keeps_missing_baseline_and_inactive_panes_idle_even_when_watermarks_differ() {
    let canonical = watermark("epoch-a", 3);
    let reconciled = watermark("epoch-a", 2);
    let derive = |active, accepted_with_baseline| {
        derive_terminal_presentation_state(TerminalPresentationInput {
            active,
            accepted_with_baseline,
            canonical: &canonical,
            reconciled: &reconciled,
            activity: None,
            now_ms: 10_000,
            not_ready_since_ms: None,
        })
    };

    assert_eq!(derive(true, false), TerminalPresentationState::Idle);
    assert_eq!(derive(false, true), TerminalPresentationState::Idle);
}

/// The unready half of the decision: an actively-viewed pane with no live view
/// reads `detached` from the grace edge on, and only while it is viewed.
#[test]
fn an_unready_viewed_pane_detaches_exactly_at_the_grace_edge() {
    let unready = watermark("epoch-a", 1);
    let derive = |active, now_ms| {
        derive_terminal_presentation_state(TerminalPresentationInput {
            active,
            accepted_with_baseline: false,
            canonical: &unready,
            reconciled: &unready,
            activity: None,
            now_ms,
            not_ready_since_ms: Some(5_000),
        })
    };

    assert_eq!(derive(true, 5_000 + DETACHED_GRACE_MS - 1), TerminalPresentationState::Idle);
    assert_eq!(derive(true, 5_000 + DETACHED_GRACE_MS), TerminalPresentationState::Detached);
    assert_eq!(derive(false, 5_000 + DETACHED_GRACE_MS), TerminalPresentationState::Idle);
}
