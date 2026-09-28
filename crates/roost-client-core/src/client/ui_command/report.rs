//! This tab's UI state report: its route and its portable pane layout, and
//! the cadence the report goes out on.
//!
//! Ports `apps/web/src/lib/uiStateReport.ts`. The UI bridge (roost-web SHELL)
//! builds the report with `build_ui_state_report`, sends it as
//! `client::rpc::calls::ui_state::UiReportState`, and drives
//! `UiStateReportCadence` from its route, layout-commit, visibility and timer
//! hooks. Only coordinator-admitted session ids cross the wire.

use super::membership::{open_ui_session, project_folder_membership};
use crate::store::Store;
use crate::store::layout::{
    LayoutDocumentError, LayoutRecords, PaneIdSource, export_layout_document,
};
use crate::store::paths::WorkerPaths;
use roost_protocol::layout::LayoutDocumentV1;

/// Every trigger in this window coalesces into one trailing send.
pub const UI_STATE_REPORT_DEBOUNCE_MS: u64 = 300;
/// A tab that changes nothing still re-reports this often.
pub const UI_STATE_REPORT_HEARTBEAT_MS: u64 = 60_000;

/// The report `UiReportState` carries.
#[derive(Debug, Clone, PartialEq)]
pub struct UiStateReport {
    /// This tab's id.
    pub tab_id: String,
    /// The route, or empty for a `/s/` route whose session the fleet has not
    /// admitted: a client-minted id must not reach the coordinator.
    pub active_path: String,
    /// The folder bucket the document belongs to, or empty off-terminal.
    pub folder_key: String,
    /// The folder's arrangement, bound to admitted sessions only; `None`
    /// off-terminal.
    pub layout_document: Option<LayoutDocumentV1>,
}

/// Where this tab is, as the host's route table resolved it.
#[derive(Debug, Clone, Copy)]
pub struct UiReportRoute<'route> {
    /// This tab's id.
    pub tab_id: &'route str,
    /// The live router pathname.
    pub path: &'route str,
    /// The session the route resolves to, if any.
    pub active_session_id: Option<&'route str>,
}

/// The route's session, only when it is open and coordinator-admitted: the
/// value whose arrival owes a re-report.
pub fn authoritative_ui_report_session_id(
    store: &Store,
    paths: &dyn WorkerPaths,
    active_session_id: Option<&str>,
) -> Option<String> {
    open_ui_session(store, paths, active_session_id?)
        .filter(|session| session.authoritative)
        .map(|session| session.session_id)
}

/// Whether an unchanged route gaining its authoritative session owes a report.
/// `previous` is `None` before the first observation.
pub fn session_resolution_owes_report(
    current: Option<&str>,
    previous: Option<Option<&str>>,
) -> bool {
    current.is_some() && previous.flatten().is_none()
}

/// Build the typed report. `Err` only when this tab's own layout cannot be
/// exported; the arrangement then stays browser-local and nothing is sent.
pub fn build_ui_state_report(
    store: &Store,
    paths: &dyn WorkerPaths,
    records: &LayoutRecords,
    ids: &mut dyn PaneIdSource,
    route: UiReportRoute<'_>,
) -> Result<UiStateReport, LayoutDocumentError> {
    let active = route
        .active_session_id
        .and_then(|session_id| open_ui_session(store, paths, session_id));
    let authoritative = active.as_ref().is_some_and(|session| session.authoritative);
    let direct_session_path = route.path.starts_with("/s/");
    let active_path = if direct_session_path && !authoritative {
        String::new()
    } else {
        route.path.to_owned()
    };
    let (folder_key, layout_document) = match active {
        Some(session) => {
            let live = project_folder_membership(store, paths, &session.folder_key)
                .authoritative_session_ids;
            let layout = records.resolve(&session.folder_key, &live, ids);
            let document = export_layout_document(&session.folder_key, &live, &layout)
                .inspect_err(|error| {
                    tracing::warn!(target: "ui_cc", folder_key = %session.folder_key, reason = %error, "ui state report layout did not export");
                })?;
            (session.folder_key, Some(document))
        }
        None => (String::new(), None),
    };
    Ok(UiStateReport {
        tab_id: route.tab_id.to_owned(),
        active_path,
        folder_key,
        layout_document,
    })
}

/// The trailing debounce and the heartbeat, as instants the host's timer
/// reads. Time is the host's: every call takes `now_ms`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UiStateReportCadence {
    send_at_ms: Option<u64>,
    heartbeat_at_ms: Option<u64>,
}

impl UiStateReportCadence {
    /// Not reporting until `start`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Begin reporting: the tab exists, so one report is owed now.
    pub fn start(&mut self, now_ms: u64) {
        self.heartbeat_at_ms = Some(now_ms.saturating_add(UI_STATE_REPORT_HEARTBEAT_MS));
        self.request(now_ms);
        tracing::info!(target: "ui_cc", "ui state reporting started");
    }

    /// Stop reporting; a pending send is dropped.
    pub fn stop(&mut self) {
        *self = Self::default();
        tracing::info!(target: "ui_cc", "ui state reporting stopped");
    }

    /// A trigger: restart the debounce window. Ignored before `start`, as a
    /// layout commit on a page without the bridge is.
    pub fn request(&mut self, now_ms: u64) {
        if self.heartbeat_at_ms.is_some() {
            self.send_at_ms = Some(now_ms.saturating_add(UI_STATE_REPORT_DEBOUNCE_MS));
        }
    }

    /// Whether a report is due at `now_ms`. Consumes the due send, and folds a
    /// passed heartbeat into a fresh debounce window.
    pub fn take_due(&mut self, now_ms: u64) -> bool {
        if let Some(heartbeat_at) = self.heartbeat_at_ms.filter(|at| *at <= now_ms) {
            self.heartbeat_at_ms = Some(heartbeat_at.saturating_add(UI_STATE_REPORT_HEARTBEAT_MS));
            self.request(now_ms);
        }
        if self.send_at_ms.is_some_and(|at| at <= now_ms) {
            self.send_at_ms = None;
            tracing::debug!(target: "ui_cc", "ui state report due");
            return true;
        }
        false
    }

    /// When the host's timer should next call `take_due`, if ever.
    pub fn next_wake_ms(&self) -> Option<u64> {
        match (self.send_at_ms, self.heartbeat_at_ms) {
            (Some(send), Some(heartbeat)) => Some(send.min(heartbeat)),
            (send, heartbeat) => send.or(heartbeat),
        }
    }
}
