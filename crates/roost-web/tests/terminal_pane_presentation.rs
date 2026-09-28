//! What the pane shows while it opens and when it dies: the startup notice for
//! each lease state, the gate that keeps a painted pane's frame through a lease
//! refresh, the offline accusation that spends its silent re-claims first, and
//! the startup card's clock. Ports `apps/web/tests/cellTerminalPresentation.test.ts`
//! ("waits for a non-null view status"), `apps/web/tests/offlineWatch.test.ts` and
//! the notice cases of `TerminalStartupOverlay.tsx`.

use roost_web::components::terminal::offline_watch::{OFFLINE_GRACE_MS, OfflineFire, OfflineWatch};
use roost_web::components::terminal::pane_status::{
    LoadingGate, PaneViewStatus, loading_notice, terminal_viewport_loading_notice,
};
use roost_web::components::terminal::startup_overlay_state::{
    FINISH_GRACE_MS, FINISH_HOLD_MS, StartupOverlayState,
};
use roost_web_terminal::startup_progress::TerminalStartupStage;
use roost_web_terminal::terminal_presentation::TerminalViewHandleStatus;

fn status(status: TerminalViewHandleStatus) -> Option<PaneViewStatus> {
    Some(PaneViewStatus {
        status,
        effective_cols: 80,
        effective_rows: 24,
    })
}

const READY: TerminalViewHandleStatus = TerminalViewHandleStatus::Accepted {
    active: true,
    baseline_ready: true,
};
const AWAITING_BASELINE: TerminalViewHandleStatus = TerminalViewHandleStatus::Accepted {
    active: true,
    baseline_ready: false,
};

#[test]
fn each_lease_state_names_its_startup_step() {
    let stage = |pending, lease| terminal_viewport_loading_notice(pending, lease).stage;
    assert_eq!(stage(true, None), TerminalStartupStage::Spawn);
    assert_eq!(stage(false, None), TerminalStartupStage::Measure);
    assert_eq!(stage(false, status(TerminalViewHandleStatus::Pending)), TerminalStartupStage::Viewport);
    assert_eq!(stage(false, status(AWAITING_BASELINE)), TerminalStartupStage::Frame);
    assert_eq!(stage(false, status(READY)), TerminalStartupStage::Render);
    assert_eq!(stage(false, status(TerminalViewHandleStatus::Rejected)), TerminalStartupStage::Retry);
    let frame = terminal_viewport_loading_notice(false, status(AWAITING_BASELINE));
    assert!(frame.detail.contains("80×24"), "{}", frame.detail);
}

#[test]
fn a_painted_pane_keeps_its_frame_through_a_lease_refresh() {
    let gate = LoadingGate {
        view_active: true,
        page_visible: true,
        offline: false,
        has_reconciled_frame: false,
        pending: false,
    };
    assert!(loading_notice(gate, status(READY)).is_some(), "not painted yet");
    let painted = LoadingGate { has_reconciled_frame: true, ..gate };
    assert!(loading_notice(painted, status(READY)).is_none());
    assert!(loading_notice(painted, status(AWAITING_BASELINE)).is_none());
    assert!(loading_notice(painted, status(TerminalViewHandleStatus::Pending)).is_none());
    assert!(loading_notice(painted, status(TerminalViewHandleStatus::Rejected)).is_some());
    assert!(loading_notice(LoadingGate { view_active: false, ..gate }, None).is_none());
    assert!(loading_notice(LoadingGate { offline: true, ..gate }, None).is_none());
}

#[test]
fn a_detached_viewed_pane_re_claims_twice_before_it_is_offline() {
    let mut watch = OfflineWatch::new();
    watch.update(true, true, false, 0);
    assert_eq!(watch.on_deadline(OFFLINE_GRACE_MS - 1), None);
    assert_eq!(watch.on_deadline(OFFLINE_GRACE_MS), Some(OfflineFire::Retry));
    assert_eq!(watch.on_deadline(2 * OFFLINE_GRACE_MS), Some(OfflineFire::Retry));
    assert!(!watch.offline());
    assert_eq!(watch.on_deadline(3 * OFFLINE_GRACE_MS), Some(OfflineFire::Offline));
    assert!(watch.offline());
    // A painted frame proves the view delivers: the accusation is withdrawn.
    assert!(watch.update(true, true, true, 4 * OFFLINE_GRACE_MS));
    assert!(!watch.offline());
}

#[test]
fn a_quiet_but_deliverable_or_unviewed_pane_is_never_accused() {
    let mut watch = OfflineWatch::new();
    watch.update(true, false, false, 0);
    assert_eq!(watch.next_deadline_ms(), None);
    watch.update(false, true, false, 0);
    assert_eq!(watch.next_deadline_ms(), None);
    // Repeated identical input does not restart the grace.
    watch.update(true, true, false, 0);
    watch.update(true, true, false, 1_000);
    assert_eq!(watch.next_deadline_ms(), Some(OFFLINE_GRACE_MS));
}

#[test]
fn the_card_holds_across_a_one_frame_gap_and_finishes_at_100() {
    let notice = |stage| {
        let mut notice = terminal_viewport_loading_notice(false, None);
        notice.stage = stage;
        notice
    };
    let mut card = StartupOverlayState::default();
    card.set_notice(Some(notice(TerminalStartupStage::Viewport)), 0);
    assert!(card.percent() >= 70.0);
    card.set_notice(None, 100);
    card.tick(100 + FINISH_GRACE_MS - 1, true);
    assert!(!card.finishing());
    card.set_notice(Some(notice(TerminalStartupStage::Frame)), 150);
    card.set_notice(None, 200);
    card.tick(200 + FINISH_GRACE_MS, true);
    assert!(card.finishing());
    assert_eq!(card.percent(), 100.0);
    let finish_started = 200 + FINISH_GRACE_MS;
    card.tick(finish_started + FINISH_HOLD_MS, true);
    assert!(card.held().is_none());
}

#[test]
fn the_meter_never_rewinds_when_a_lease_regresses() {
    let mut card = StartupOverlayState::default();
    let mut frame = terminal_viewport_loading_notice(false, status(AWAITING_BASELINE));
    card.set_notice(Some(frame.clone()), 0);
    card.tick(2_000, true);
    let reached = card.percent();
    frame.stage = TerminalStartupStage::Viewport;
    card.set_notice(Some(frame), 2_100);
    assert!(card.percent() >= reached, "{} < {reached}", card.percent());
}
