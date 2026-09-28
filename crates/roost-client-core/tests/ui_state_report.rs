//! This tab's typed UI state report at the wire boundary, and its cadence.
//!
//! Ports `apps/web/tests/uiStateReport.test.ts`: the active folder exports as a
//! portable `LayoutDocumentV1`, optimistic client-only ids stay local, and an
//! off-terminal route reports no layout. v2's two Solid hydration tests pin
//! `scheduleUiStateReportOnSessionResolution`; here that is the pure
//! `session_resolution_owes_report`, driven through the same value sequences.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod layout_support;
mod ui_command_support;

use roost_client_core::client::rpc::calls::ui_state::UiReportState;
use roost_client_core::client::rpc::unary::UnaryMethod;
use roost_client_core::client::ui_command::{
    UI_STATE_REPORT_DEBOUNCE_MS, UI_STATE_REPORT_HEARTBEAT_MS, UiReportRoute, UiStateReport,
    UiStateReportCadence, authoritative_ui_report_session_id, build_ui_state_report,
    session_resolution_owes_report,
};
use roost_client_core::store::Store;
use roost_client_core::store::layout::LayoutRecords;
use roost_client_core::store::optimistic_spawn::{begin_optimistic_spawn, settle_spawn_rejected};
use roost_client_core::store::paths::ExactWorkerPaths;
use roost_proto::buffa::Message;
use roost_protocol::proto_adapters::layout_document_proto::layout_document_from_proto;

use layout_support::{CountedIds, single_pane_document};
use ui_command_support::*;

fn report(store: &Store, path: &str, active_session_id: Option<&str>) -> UiStateReport {
    let records = LayoutRecords::new();
    let mut ids = CountedIds::new("pane");
    build_ui_state_report(
        store,
        &ExactWorkerPaths,
        &records,
        &mut ids,
        UiReportRoute { tab_id: "tab-current", path, active_session_id },
    )
    .unwrap()
}

/// The request bytes, decoded back as the coordinator reads them.
fn on_the_wire(report: &UiStateReport) -> (Vec<u8>, roost_proto::UiReportStateRequest) {
    let bytes = UiReportState { report: report.clone() }.encode_request().unwrap();
    let request = roost_proto::UiReportStateRequest::decode_from_slice(&bytes).unwrap();
    (bytes, request)
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle.as_bytes())
}

#[test]
fn exports_the_active_layout_as_a_typed_document() {
    let core = seeded_core();
    let path = format!("/s/{ALPHA}");
    let built = report(core.store(), &path, Some(ALPHA));
    assert_eq!(built.tab_id, "tab-current");
    assert_eq!(built.active_path, path);
    assert_eq!(built.folder_key, work_folder());
    assert_eq!(built.layout_document, Some(single_pane_document(&[ALPHA, BETA], ALPHA)));

    let (_, request) = on_the_wire(&built);
    assert_eq!(request.tab_id, "tab-current");
    assert_eq!(request.active_path, path);
    assert_eq!(request.folder_key, work_folder());
    let document = layout_document_from_proto(request.layout_document.as_option().unwrap()).unwrap();
    assert_eq!(Some(document), built.layout_document);
}

#[test]
fn omits_a_pending_spawn_identity_while_exporting_authoritative_siblings() {
    let mut core = seeded_core();
    begin_optimistic_spawn(core.store_mut(), PENDING, MACHINE, "/work", None, 150).unwrap();
    let built = report(core.store(), &format!("/s/{PENDING}"), Some(PENDING));
    assert_eq!(built.active_path, "");
    assert_eq!(built.folder_key, work_folder());
    let document = built.layout_document.as_ref().unwrap();
    let bound: Vec<&str> = document.bindings.iter().map(|binding| binding.session_id.as_str()).collect();
    assert_eq!(bound, vec![ALPHA, BETA]);
    let (bytes, _) = on_the_wire(&built);
    assert!(!contains(&bytes, PENDING), "a client-minted id crossed the wire");
}

#[test]
fn scrubs_a_failed_optimistic_session_path_after_its_placeholder_is_removed() {
    let mut core = seeded_core();
    let ticket = begin_optimistic_spawn(core.store_mut(), PENDING, MACHINE, "/work", None, 150).unwrap();
    settle_spawn_rejected(core.store_mut(), &ticket, "admission rejected", 160);
    let built = report(core.store(), &format!("/s/{PENDING}"), Some(PENDING));
    assert_eq!(built.active_path, "");
    assert_eq!(built.folder_key, "");
    assert_eq!(built.layout_document, None);
    let (bytes, request) = on_the_wire(&built);
    assert!(!contains(&bytes, PENDING));
    assert!(request.layout_document.is_unset());
}

#[test]
fn deferred_hydration_owes_exactly_one_authoritative_report() {
    let path = format!("/s/{ALPHA}");
    // Before the session is synced the route resolves to nothing admitted.
    let empty = roost_client_core::ClientCore::in_memory(OWN_TAB);
    let unresolved = authoritative_ui_report_session_id(empty.store(), &ExactWorkerPaths, Some(ALPHA));
    assert_eq!(unresolved, None);
    assert_eq!(report(empty.store(), &path, Some(ALPHA)).active_path, "");

    // The row lands: the one change after the deferred first read.
    let synced = seeded_core();
    let resolved = authoritative_ui_report_session_id(synced.store(), &ExactWorkerPaths, Some(ALPHA));
    assert_eq!(resolved.as_deref(), Some(ALPHA));
    assert!(session_resolution_owes_report(resolved.as_deref(), None));
    let hydrated = report(synced.store(), &path, Some(ALPHA));
    assert_eq!(hydrated.active_path, path);
    assert_eq!(hydrated.folder_key, work_folder());
    assert!(hydrated.layout_document.is_some());
}

#[test]
fn deferred_hydration_skips_a_tracked_null_and_reports_the_resolution_after_it() {
    // v2's observed sequence: first change to null (previous undefined), then
    // to the session (previous null). Only the second owes a report.
    assert!(!session_resolution_owes_report(None, None));
    assert!(session_resolution_owes_report(Some(ALPHA), Some(None)));
    // A route already resolved does not re-report on a later swap: the route
    // trigger owns that.
    assert!(!session_resolution_owes_report(Some(BETA), Some(Some(ALPHA))));
}

#[test]
fn reports_an_off_terminal_path_without_a_layout() {
    let core = seeded_core();
    let built = report(core.store(), "/settings/machines", None);
    assert_eq!(built.tab_id, "tab-current");
    assert_eq!(built.active_path, "/settings/machines");
    assert_eq!(built.folder_key, "");
    assert_eq!(built.layout_document, None);
}

#[test]
fn triggers_inside_the_debounce_window_coalesce_into_one_trailing_send() {
    let mut cadence = UiStateReportCadence::new();
    cadence.request(0);
    assert_eq!(cadence.next_wake_ms(), None, "no report before the bridge starts");

    cadence.start(1_000);
    cadence.request(1_100);
    cadence.request(1_200);
    assert!(!cadence.take_due(1_200 + UI_STATE_REPORT_DEBOUNCE_MS - 1));
    assert!(cadence.take_due(1_200 + UI_STATE_REPORT_DEBOUNCE_MS));
    assert!(!cadence.take_due(1_200 + UI_STATE_REPORT_DEBOUNCE_MS), "one send per window");

    let heartbeat = 1_000 + UI_STATE_REPORT_HEARTBEAT_MS;
    assert_eq!(cadence.next_wake_ms(), Some(heartbeat));
    assert!(!cadence.take_due(heartbeat));
    assert!(cadence.take_due(heartbeat + UI_STATE_REPORT_DEBOUNCE_MS), "the heartbeat re-reports");

    cadence.stop();
    cadence.request(heartbeat + 1_000);
    assert_eq!(cadence.next_wake_ms(), None);
}
