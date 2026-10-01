//! This tab's UI state report, and the triggers that owe one.
//!
//! Ports `apps/web/src/lib/uiStateReport.ts` and the reporting half of
//! `apps/web/src/components/UiBridge.tsx`. The cadence is
//! `client::ui_command::UiStateReportCadence` and the payload is
//! `build_ui_state_report`; this file is only the decision around them: what the
//! report would carry, whether that moved, and when a send is due.
//!
//! FOUR TRIGGERS, ONE QUESTION. v2 asked for a report on a route change, on a
//! layout commit, on the route's session becoming coordinator-admitted, and on
//! the tab returning to the foreground. Every one of those changes what
//! `build_ui_state_report` returns, so this asks one question instead — has the
//! report's input moved? — and the four cases cannot drift out of step with the
//! payload the way four separate subscriptions can.

use roost_client_core::client::ui_command::{
    OpenUiSession, UiReportRoute, UiStateReport, UiStateReportCadence, build_ui_state_report,
    open_ui_session, project_folder_membership,
};
use roost_client_core::store::WorkerPaths;
use roost_client_core::store::layout::{PaneIdSource, PaneLayout};

use crate::ui_bridge::host::UiBridgeHost;

/// What the report would carry, minus the document itself.
#[derive(Debug, Clone, PartialEq)]
struct ReportInputs {
    path: String,
    active: Option<OpenUiSession>,
    live_session_ids: Vec<String>,
    stored: Option<PaneLayout>,
}

/// This tab's reporting state: the cadence, and the last input a report owed.
#[derive(Debug, Default)]
pub struct UiStateReporter {
    cadence: UiStateReportCadence,
    observed: Option<ReportInputs>,
    pane_ids: ReportPaneIds,
}

/// The pane ids a report's resolve mints from.
///
/// NOT the deck's. `DeckState` hands its records and its id source out
/// together, mutably, while `build_ui_state_report` needs the whole store
/// shared, and a report that reached for the deck's counter would advance the
/// ids the deck persists for a read that throws them away. A document key is a
/// POSITION in a preorder walk (`store::layout::document::export`), so these ids
/// are minted, exported away as `leaf-N`/`slot-N`, and never stored or painted.
#[derive(Debug, Default)]
struct ReportPaneIds {
    minted: u64,
}

impl PaneIdSource for ReportPaneIds {
    fn mint_pane_id(&mut self) -> String {
        self.minted += 1;
        format!("report-pane-{}", self.minted)
    }
}

impl UiStateReporter {
    /// A reporter that owes the tab's first report.
    pub fn new() -> Self {
        Self::default()
    }

    /// Begin reporting: the tab exists, so one report is owed now.
    pub fn start(&mut self, now_ms: u64) {
        self.observed = None;
        self.cadence.start(now_ms);
    }

    /// Stop reporting; a pending send is dropped.
    pub fn stop(&mut self) {
        self.observed = None;
        self.cadence.stop();
    }

    /// When the host's next sweep owes a send, if one is owed at all.
    pub fn next_wake_ms(&self) -> Option<u64> {
        self.cadence.next_wake_ms()
    }

    /// Run one sweep: request a send if this tab's state moved, then send if
    /// the cadence says one is due. `true` when a report went out.
    ///
    /// The request happens on the move, not on the send, so a burst that keeps
    /// moving coalesces into one trailing report however many sweeps it spans.
    pub fn tick(
        &mut self,
        host: &mut dyn UiBridgeHost,
        store: &roost_client_core::Store,
        paths: &dyn WorkerPaths,
        path: &str,
        now_ms: u64,
    ) -> bool {
        let inputs = read_inputs(store, paths, path);
        if self.observed.as_ref() != Some(&inputs) {
            self.observed = Some(inputs);
            self.cadence.request(now_ms);
        }
        if !self.cadence.take_due(now_ms) {
            return false;
        }
        if store.tab_id.is_empty() {
            // The coordinator refuses a report whose body names no tab id
            // (`tab_id` is a required field, as in v2's `handlers-ui.ts`), and
            // `pump::boot` claims that id before this document opens a socket,
            // so a report here could not name its tab. Dropping the due send is
            // deliberate: the heartbeat is still armed, so the first sweep after
            // the claim reports normally.
            tracing::warn!(target: "ui_cc", "this document has claimed no tab id; the ui state report is not sent");
            return false;
        }
        let report = build_report(store, paths, path, &mut self.pane_ids);
        host.send_report(report);
        true
    }
}

/// What the report at `path` would be built from, as a value to compare.
fn read_inputs(
    store: &roost_client_core::Store,
    paths: &dyn WorkerPaths,
    path: &str,
) -> ReportInputs {
    let active = crate::route_session::active_session_for_path(store, paths, path)
        .and_then(|session| open_ui_session(store, paths, session.id.as_str()));
    let (live_session_ids, stored) = match &active {
        Some(session) => {
            let live = project_folder_membership(store, paths, &session.folder_key)
                .authoritative_session_ids;
            let stored = store.deck.records().stored(&session.folder_key).cloned();
            (live, stored)
        }
        None => (Vec::new(), None),
    };
    ReportInputs {
        path: path.to_owned(),
        active,
        live_session_ids,
        stored,
    }
}

/// The report, or the honest empty one when this tab's layout will not export.
///
/// A layout this browser cannot export stays LOCAL rather than poisoning the
/// timer: the reader still paints it, and the coordinator keeps the last
/// arrangement it admitted until the next report this tab can build.
fn build_report(
    store: &roost_client_core::Store,
    paths: &dyn WorkerPaths,
    path: &str,
    ids: &mut dyn PaneIdSource,
) -> UiStateReport {
    let route = UiReportRoute {
        tab_id: store.tab_id.as_str(),
        path,
        active_session_id: crate::route_session::active_session_for_path(store, paths, path)
            .map(|session| session.id.as_str()),
    };
    let records = store.deck.records();
    build_ui_state_report(store, paths, records, ids, route).unwrap_or_else(|error| {
        tracing::warn!(target: "ui_cc", %error, "this tab's layout cannot be exported; the arrangement stays browser-local");
        UiStateReport {
            tab_id: store.tab_id.clone(),
            active_path: path.to_owned(),
            folder_key: String::new(),
            layout_document: None,
        }
    })
}
