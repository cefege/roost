//! The desktop status bar: the active session's
//! machine, its agent, the session's context, and the fleet counts. Ported from
//! `apps/web/src/components/layout/WorkbenchStatusBar.tsx`.
//!
//! It projects state the store ALREADY holds. Nothing here is invented: there is
//! no synthetic source-control status, no "connected" claim the transport has
//! not made, and no item whose absence would read as a fault.
//!
//! THE COORDINATOR ITEM IS ABSENT, DELIBERATELY. v2 reads a health snapshot the
//! sync socket publishes; this port has no such publisher. The previous shape
//! hard-coded `last_attempt_failed: false` with no `last_success_ms`, so
//! `coordinator_state` could only ever answer `Syncing` — a status bar
//! permanently grey and permanently mid-sync, which is a claim about the system
//! that is not true. `shell_metrics::coordinator_state` is still there, still
//! pure and still tested, and the item returns the moment a health source exists
//! to feed it; nothing has to be re-derived when it does.
//!
//! The agent word IS shown, because it has a real source:
//! `roost_client_core`'s own presentation table, so the status bar and the
//! agent sidebar chips cannot spell "needs input" two ways.

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::ClientCore;
use roost_client_core::client::agents::{
    AgentStatusLevel, agent_status_presentation, derive_agent_status_level,
};
use roost_client_core::store::navigation::worker_online;
use roost_protocol::wire::SessionStatus;

use super::shell_metrics::{
    CoordinatorState, coordinator_health_from_link, coordinator_state, session_context,
    workbench_title,
};
use crate::components::md::StatusDot;
use crate::platform::visibility::page_visible;

/// One dot-and-word reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    /// The word beside the dot.
    pub label: String,
    /// The design-system status the dot shows.
    pub status: String,
}

/// Everything the bar reads, taken in one pass so its items cannot be drawn from
/// two different moments of the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusReadings {
    /// What the bar says about the coordinator.
    ///
    /// ALWAYS DRAWN. v2 reads a health snapshot a five-second `misc.health`
    /// poller publishes on `window`; this port has no poller, and does not need
    /// one — the live Sync link carries the same evidence continuously (see
    /// `coordinator_health_from_link`). Gating the item on having a source is
    /// what left the previous port with no `workbench-status-sync` element at
    /// all: an item that vanishes is indistinguishable from a healthy one, and
    /// `Syncing` forever is not a claim anyone can act on.
    pub coordinator: CoordinatorState,
    /// The active session's machine, when the route names a session.
    pub machine: Option<Reading>,
    /// The active session's agent, when it has a status worth showing.
    pub agent: Option<Reading>,
    /// The active session's title and folder, as one string.
    pub context: Option<String>,
    /// How many sessions are open.
    pub open_sessions: usize,
    /// How many workers are online.
    pub workers_online: usize,
    /// How many workers are registered.
    pub workers_total: usize,
    /// The coordinator build's short revision, once identity is known.
    pub revision: Option<String>,
}

impl StatusReadings {
    /// `1 session` / `3 sessions`. The singular is spelled out rather than left
    /// to a plural rule, because the bar sits in a fixed grid area and a stray
    /// `s` is the kind of thing that gets noticed.
    pub fn sessions_worded(&self) -> String {
        if self.open_sessions == 1 {
            "1 session".to_string()
        } else {
            format!("{} sessions", self.open_sessions)
        }
    }

    /// `2/4 workers` — online over registered, because the registered count is
    /// the one that says something is wrong.
    pub fn workers_worded(&self) -> String {
        format!("{}/{} workers", self.workers_online, self.workers_total)
    }
}

/// The status bar, shown on every desktop path.
#[component]
pub fn StatusBar() -> Element {
    let core = crate::pump::use_store().core();
    let path = crate::router_state::use_location()();
    let now_ms = core.borrow().clock().now_ms() as i64;
    let readings = read_status(&path, &core, now_ms);
    let coordinator = readings.coordinator;
    let machine = readings.machine.clone();
    let agent = readings.agent.clone();
    let context = readings.context.clone();
    // The focused session's shell variables (OSC 1337 SetUserVar) ride the
    // bar: the sidebar's folder rows aggregate sessions and the tab is narrow.
    let focused_session = crate::route_session::active_session_for_path(
        core.borrow().store(),
        &crate::platform::worker_paths::BrowserWorkerPaths,
        &path,
    )
    .map(|session| session.id.as_str().to_owned());
    rsx! {
        footer {
            class: "workbench-status-bar",
            "data-testid": "workbench-status-bar",
            "aria-label": "Workbench status",
            div { class: "workbench-status-bar__left",
            span {
                class: "workbench-status-item",
                "data-testid": "workbench-status-sync",
                "data-status": coordinator.status(),
                StatusDot { status: coordinator.status().to_string() }
                span { {coordinator.label()} }
            }
                if let Some(reading) = machine {
                    span {
                        class: "workbench-status-item workbench-status-item--optional",
                        "data-testid": "workbench-status-worker",
                        StatusDot { status: reading.status.clone() }
                        span { {reading.label} }
                    }
                }
                if let Some(reading) = agent {
                    span {
                        class: "workbench-status-item workbench-status-item--optional",
                        "data-testid": "workbench-status-agent",
                        StatusDot { status: reading.status.clone() }
                        span { {reading.label} }
                    }
                }
                if let Some(context) = context {
                    span {
                        class: "workbench-status-item workbench-status-item--context",
                        "data-testid": "workbench-status-context",
                        {context}
                    }
                }
                if let Some(session_id) = focused_session {
                    crate::components::terminal_signal_marks::TerminalUserVarBadges { session_id }
                }
            }
            div { class: "workbench-status-bar__right",
                span {
                    class: "workbench-status-item",
                    "data-testid": "workbench-status-counts",
                    {readings.sessions_worded()}
                    span { "aria-hidden": "true", "·" }
                    {readings.workers_worded()}
                }
                if let Some(revision) = readings.revision.clone() {
                    span {
                        class: "workbench-status-item workbench-status-item--revision",
                        "data-testid": "workbench-status-revision",
                        {revision}
                    }
                }
            }
        }
    }
}

/// Read the whole bar from the store in one borrow.
///
/// `now_ms` is the caller's clock, not a read here: the bar's staleness window
/// is a rule with a boundary, and a rule that reads the clock inside itself is a
/// rule that can only be tested by waiting.
fn read_status(path: &str, core: &Rc<RefCell<ClientCore>>, now_ms: i64) -> StatusReadings {
    let borrowed = core.borrow();
    let store = borrowed.store();
    let _ = store.account_id.as_deref();
    let clock_ms = now_ms.max(0) as u64;
    let coordinator = coordinator_state(
        store.coord_identity.is_some(),
        coordinator_health_from_link(
            store.sync.idle_ms(clock_ms),
            clock_ms,
            !crate::platform::network::browser_online(),
            page_visible(),
        ),
    );
    let open_sessions = store
        .sessions
        .sessions()
        .values()
        .filter(|session| session.status == SessionStatus::Open)
        .count();
    let workers_total = store.workers.len();
    let workers_online = store
        .workers
        .values()
        .filter(|worker| worker_online(worker, store.routable_worker_fps.as_ref(), now_ms))
        .count();
    let active = crate::route_session::active_session_for_path(
        store,
        &crate::platform::worker_paths::BrowserWorkerPaths,
        path,
    );
    let context = active.map(|session| {
        let folder = session.spawn_cwd.as_deref().unwrap_or(session.cwd.as_str());
        let chrome = super::app_shell::session_chrome(store, session);
        let title = workbench_title(
            path,
            chrome.title.as_deref(),
            chrome.folder.as_deref().or(Some("")),
        );
        session_context(&title, Some(folder))
    });
    let machine = active.and_then(|session| {
        let worker = store.workers.get(session.worker_fp.as_str())?;
        Some(Reading {
            label: if worker.label.is_empty() {
                worker.fp.as_str().to_string()
            } else {
                worker.label.clone()
            },
            status: if worker_online(worker, store.routable_worker_fps.as_ref(), now_ms) {
                "ok".to_string()
            } else {
                "offline".to_string()
            },
        })
    });
    let agent = active.and_then(|session| {
        let status = store.agent_status.status(&session.id)?;
        let level = derive_agent_status_level(
            Some(status),
            Some(store.agent_seen.acknowledged_revision(status)),
        );
        if level == AgentStatusLevel::Unknown {
            return None;
        }
        let presentation = agent_status_presentation(level);
        Some(Reading {
            label: presentation.label.to_string(),
            status: agent_dot_status(presentation.dot_status).to_string(),
        })
    });
    StatusReadings {
        coordinator,
        machine,
        agent,
        context,
        open_sessions,
        workers_online,
        workers_total,
        revision: store
            .coord_identity
            .as_ref()
            .map(|identity| identity.git_sha.chars().take(7).collect()),
    }
}

/// The design-system status an agent level's dot shows.
///
/// The core's `AgentDotStatus` is a four-member vocabulary of its own; this is
/// the one place it is spelled as the status names the dot's stylesheet knows,
/// so the mapping is stated once instead of at every call site.
fn agent_dot_status(dot: roost_client_core::client::agents::AgentDotStatus) -> &'static str {
    match dot {
        roost_client_core::client::agents::AgentDotStatus::Warn => "warn",
        roost_client_core::client::agents::AgentDotStatus::Ok => "ok",
        roost_client_core::client::agents::AgentDotStatus::Info => "info",
        roost_client_core::client::agents::AgentDotStatus::Idle => "idle",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use roost_client_core::ClientCore;

    fn bar(open: usize, online: usize, total: usize) -> StatusReadings {
        StatusReadings {
            coordinator: CoordinatorState::Synced,
            machine: None,
            agent: None,
            context: None,
            open_sessions: open,
            workers_online: online,
            workers_total: total,
            revision: None,
        }
    }

    #[test]
    fn one_session_is_singular_and_none_is_plural() {
        assert_eq!(bar(1, 0, 0).sessions_worded(), "1 session");
        assert_eq!(bar(0, 0, 0).sessions_worded(), "0 sessions");
        assert_eq!(bar(4, 0, 0).sessions_worded(), "4 sessions");
    }

    #[test]
    fn the_counts_read_online_over_registered() {
        // The registered total is the one that says something is wrong; leading
        // with it would bury the count a reader is actually looking for.
        assert_eq!(bar(2, 2, 4).workers_worded(), "2/4 workers");
    }

    #[test]
    fn an_empty_fleet_reads_as_zero_of_zero_rather_than_as_nothing() {
        // A missing counts item reads as a broken bar; `0/0 workers` reads as a
        // fleet that has not registered yet, which is what it is.
        assert_eq!(bar(0, 0, 0).workers_worded(), "0/0 workers");
    }

    #[test]
    fn every_agent_dot_status_is_a_name_the_dot_stylesheet_knows() {
        for dot in [
            roost_client_core::client::agents::AgentDotStatus::Warn,
            roost_client_core::client::agents::AgentDotStatus::Ok,
            roost_client_core::client::agents::AgentDotStatus::Info,
            roost_client_core::client::agents::AgentDotStatus::Idle,
        ] {
            let name = agent_dot_status(dot);
            assert!(
                crate::components::md::status_dot::status_dot_token(name).starts_with("--"),
                "{name} is not a status the dot styles a rule for"
            );
        }
    }

    #[test]
    fn a_booted_client_with_no_link_reads_as_unreachable_rather_than_nothing() {
        // A boot has no link yet and no identity, and the item is still drawn.
        // An item that VANISHES is indistinguishable from a healthy one, which
        // is the failure this replaces: the bar used to have no coordinator
        // reading at all, so the element the smoke looks for was never in the
        // page.
        let core = ClientCore::in_memory("tab-test");
        let readings = read_status("/", &Rc::new(RefCell::new(core)), 1_000);
        assert_eq!(readings.coordinator, CoordinatorState::Unreachable);
    }

    #[test]
    fn a_path_that_names_no_session_shows_no_machine_and_no_context() {
        // A status bar inventing a machine for a settings page would be a claim
        // about a session the reader is not looking at.
        let core = ClientCore::in_memory("tab-test");
        let readings = read_status("/settings/machines", &Rc::new(RefCell::new(core)), 1_000);
        assert_eq!(readings.machine, None);
        assert_eq!(readings.agent, None);
        assert_eq!(readings.context, None);
    }

    #[test]
    fn a_folder_terminal_route_shows_no_machine_rather_than_a_guessed_one() {
        // `/t/:workerFp/*folderPath` names a folder. A machine item here would be
        // about a session the chrome never resolved.
        let core = ClientCore::in_memory("tab-test");
        let readings = read_status("/t/fp12345/src", &Rc::new(RefCell::new(core)), 1_000);
        assert_eq!(readings.machine, None);
    }
}
