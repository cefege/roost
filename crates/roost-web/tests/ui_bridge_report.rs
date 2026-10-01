//! The report half of the shell's UI bridge: when one is owed, when one goes
//! out, and what it carries.
//!
//! The gate these pin is `smoke/terminal/ui-layout-apply.spec.ts:103`, whose
//! poll is `if (!state?.layoutDocument) return null` — a tab that never reports
//! leaves the coordinator holding no arrangement for it, and every layout read
//! against that tab reads `null`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod ui_bridge_support;

use roost_client_core::client::ui_command::{
    UI_STATE_REPORT_DEBOUNCE_MS, UI_STATE_REPORT_HEARTBEAT_MS,
};
use roost_protocol::layout::LayoutDocumentV1;

use ui_bridge_support::*;

/// The path the two seeded sessions resolve through.
fn session_path(session_id: &str) -> String {
    format!("/s/{session_id}")
}

/// The session ids a document binds, in slot order.
fn bound(document: &LayoutDocumentV1) -> Vec<String> {
    document
        .bindings
        .iter()
        .map(|binding| binding.session_id.clone())
        .collect()
}

/// A bridge on a terminal route, already past its first report.
fn reporting_bridge() -> Bridge {
    let bridge = mounted_bridge();
    bridge.show(&session_path(ALPHA), false);
    bridge.state.borrow_mut().start(0);
    bridge.run_at(0);
    bridge.run_at(UI_STATE_REPORT_DEBOUNCE_MS);
    assert_eq!(
        bridge.host.borrow().reports.len(),
        1,
        "the tab exists, so its first report is owed and then sent"
    );
    bridge.host.borrow_mut().clear();
    bridge
}

/// THE SMOKE GATE. A tab showing a terminal route reports the route, the folder
/// bucket, and the arrangement as a portable document bound to the sessions the
/// fleet knows.
#[test]
fn a_terminal_route_reports_its_path_and_its_portable_layout() {
    let bridge = reporting_bridge();
    // The heartbeat opens a debounce window of its own, so it takes two
    // sweeps: one to fold it, one to send.
    bridge.run_at(UI_STATE_REPORT_HEARTBEAT_MS);
    bridge.run_at(UI_STATE_REPORT_HEARTBEAT_MS + UI_STATE_REPORT_DEBOUNCE_MS);
    let reports = bridge.host.borrow().reports.clone();
    let report = reports
        .last()
        .expect("the heartbeat re-reports an unchanged tab");

    assert_eq!(report.tab_id, OWN_TAB, "the report is fenced to this tab");
    assert_eq!(report.active_path, session_path(ALPHA));
    assert_eq!(report.folder_key, folder_key(&bridge));
    let document = report
        .layout_document
        .as_ref()
        .expect("a terminal route reports its arrangement, or the coordinator reads null");
    assert_eq!(
        bound(document),
        vec![ALPHA.to_owned(), BETA.to_owned()],
        "the document binds the folder's admitted sessions, oldest first"
    );
}

/// A debounced burst is ONE report, sent after the last trigger's window.
#[test]
fn a_burst_of_changes_coalesces_into_one_trailing_report() {
    let bridge = reporting_bridge();
    // Five route changes, each one sweep apart, so each restart of the window
    // lands before the previous one would have fired.
    for step in 1..=5u64 {
        bridge.show(
            &session_path(if step % 2 == 0 { ALPHA } else { BETA }),
            false,
        );
        bridge.run_at(step * 250);
        assert!(
            bridge.host.borrow().reports.is_empty(),
            "a change inside the debounce window owes no report yet (step {step})"
        );
    }
    let settled = 5 * 250 + UI_STATE_REPORT_DEBOUNCE_MS;
    bridge.run_at(settled);
    assert_eq!(
        bridge.host.borrow().reports.len(),
        1,
        "five triggers inside one window are one report"
    );
}

/// A tab that changes nothing still re-reports, on the heartbeat and nowhere
/// else: the coordinator has to be able to tell a live tab from a stale row.
#[test]
fn an_unchanged_tab_reports_on_the_heartbeat_and_not_before_it() {
    let bridge = reporting_bridge();
    for now in [
        UI_STATE_REPORT_DEBOUNCE_MS * 2,
        UI_STATE_REPORT_HEARTBEAT_MS / 2,
        UI_STATE_REPORT_HEARTBEAT_MS - 1,
        UI_STATE_REPORT_HEARTBEAT_MS,
    ] {
        bridge.run_at(now);
        assert!(
            bridge.host.borrow().reports.is_empty(),
            "nothing moved and the heartbeat has not fired its window: no report at {now}ms"
        );
    }
    bridge.run_at(UI_STATE_REPORT_HEARTBEAT_MS + UI_STATE_REPORT_DEBOUNCE_MS);
    assert_eq!(
        bridge.host.borrow().reports.len(),
        1,
        "the heartbeat is the only trigger a quiet tab has"
    );
}

/// THE FENCE. A document that has claimed no tab id must not report at all:
/// `ui_state::fence::require_tab_fence` refuses the request, and a report sent
/// anyway is a request the coordinator cannot attribute to a live tab socket.
#[test]
fn a_tab_with_no_claimed_id_never_sends_a_report() {
    let bridge = mounted_bridge_claiming("");
    bridge.show(&session_path(ALPHA), false);
    bridge.state.borrow_mut().start(0);
    for now in [
        0,
        UI_STATE_REPORT_DEBOUNCE_MS,
        UI_STATE_REPORT_HEARTBEAT_MS + UI_STATE_REPORT_DEBOUNCE_MS,
    ] {
        bridge.run_at(now);
    }
    assert!(
        bridge.host.borrow().reports.is_empty(),
        "the header and the body both carry the claimed id, so an unclaimed tab \
         has no report to send"
    );
}

/// A route change is a report trigger, so a reader who navigates is described
/// to the coordinator without waiting for the heartbeat.
#[test]
fn a_route_change_owes_a_report_without_touching_the_store() {
    let bridge = reporting_bridge();
    bridge.show("/settings/machines", false);
    bridge.run_at(1_000);
    assert!(
        bridge.host.borrow().reports.is_empty(),
        "the change owes a report, not an immediate one"
    );
    bridge.run_at(1_000 + UI_STATE_REPORT_DEBOUNCE_MS);
    let reports = bridge.host.borrow().reports.clone();
    let report = reports.last().expect("the route change was reported");
    assert_eq!(report.active_path, "/settings/machines");
    assert_eq!(
        report.folder_key, "",
        "off a terminal route there is no folder"
    );
    assert!(
        report.layout_document.is_none(),
        "off a terminal route there is no arrangement to report"
    );
}

/// THE WIRING. In a document the pump's own sweep is the only thing that runs
/// the bridge, so a bridge the sweep does not reach would report nothing at all
/// while every unit around it passed.
///
/// The arm is DELIBERATELY not called here. `UiStateReportCadence::request` is
/// documented as ignored before `start`, and the component never started it:
/// this test used to call `start` itself, so it exercised the wiring with the
/// one step the product was missing and passed while the tab reported nothing
/// for the life of the document. The first sweep arms the cadence now, and a
/// test that starts it by hand can no longer hide that.
#[test]
fn the_pumps_own_sweep_drives_the_bridge() {
    let bridge = mounted_bridge();
    bridge.show(&session_path(ALPHA), false);

    // Registered exactly the way `UiBridge` registers it.
    let swept = bridge.pump.clone();
    let state = bridge.state.clone();
    let host = bridge.host.clone();
    bridge.pump.on_sweep(std::rc::Rc::new(move |now_ms| {
        let mut host = host.borrow_mut();
        state.borrow_mut().sweep(&swept, &mut *host, now_ms);
    }));

    bridge.sweep_at(0);
    bridge.sweep_at(UI_STATE_REPORT_DEBOUNCE_MS);

    let reports = bridge.host.borrow().reports.clone();
    let report = reports
        .first()
        .expect("a sweep carries the report the cadence owed");
    assert_eq!(report.tab_id, OWN_TAB);
    assert_eq!(report.active_path, session_path(ALPHA));
    assert!(report.layout_document.is_some());
}

/// A removed listener stops being called, so a bridge that unmounted does not
/// keep reporting over the shell that replaced it.
#[test]
fn a_removed_sweep_listener_is_never_called_again() {
    let bridge = mounted_bridge();
    bridge.show(&session_path(ALPHA), false);
    bridge.state.borrow_mut().start(0);
    let swept = bridge.pump.clone();
    let state = bridge.state.clone();
    let host = bridge.host.clone();
    let token = bridge.pump.on_sweep(std::rc::Rc::new(move |now_ms| {
        let mut host = host.borrow_mut();
        state.borrow_mut().sweep(&swept, &mut *host, now_ms);
    }));
    bridge.sweep_at(0);
    bridge.sweep_at(UI_STATE_REPORT_DEBOUNCE_MS);
    let sent = bridge.host.borrow().reports.len();
    assert_eq!(sent, 1, "the live listener reported");

    bridge.pump.remove_sweep_listener(token);
    bridge.sweep_at(UI_STATE_REPORT_HEARTBEAT_MS + UI_STATE_REPORT_DEBOUNCE_MS);
    assert_eq!(
        bridge.host.borrow().reports.len(),
        sent,
        "a listener the shell took back is not called again"
    );
}
