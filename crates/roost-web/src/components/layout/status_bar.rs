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
use roost_protocol::wire::{Session, SessionStatus};

use super::shell_metrics::{CoordinatorState, session_context, workbench_title};
use crate::components::design_icon::StatusDot;
use crate::components::layout::app_shell::is_terminal_route;

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
    /// What the bar says about the coordinator, or `None` when this build has
    /// no health source and therefore no honest word to print.
    ///
    /// ABSENT IS THE HONEST STATE. v2 reads a health snapshot the sync socket
    /// publishes on `window`; this port has no such publisher, and the previous
    /// shape hard-coded `last_attempt_failed: false` with no `last_success_ms`,
    /// which can only ever read `Syncing` — a status bar permanently grey and
    /// permanently mid-sync is a claim about the system that is not true. An
    /// operator seeing no status can tell it apart from a healthy one; an
    /// operator seeing `Syncing` forever cannot.
    pub coordinator: Option<CoordinatorState>,
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
pub fn StatusBar(path: String) -> Element {
    let core = use_context::<Rc<RefCell<ClientCore>>>();
    let now_ms = core.borrow().clock().now_ms() as i64;
    let readings = use_hook(move || read_status(&path, &core, now_ms));
    let coordinator = readings.coordinator;
    let machine = readings.machine.clone();
    let agent = readings.agent.clone();
    let context = readings.context.clone();
    rsx! {
        footer {
            class: "workbench-status-bar",
            "data-testid": "workbench-status-bar",
            "aria-label": "Workbench status",
            div { class: "workbench-status-bar__left",
                if let Some(coordinator) = coordinator {
                    span {
                        class: "workbench-status-item",
                        "data-testid": "workbench-status-sync",
                        "data-status": coordinator.status(),
                        StatusDot { status: coordinator.status().to_string() }
                        span { {coordinator.label()} }
                    }
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
            }
            div { class: "workbench-status-bar__right",
                span {
                    class: "workbench-status-item",
                    "data-testid": "workbench-status-counts",
                    {readings.sessions_worded()}
                    span { "aria-hidden": "true", "·" }
                    {readings.workers_worded()}
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
    let _ = (store.account_id.as_deref(), page_visible(), now_ms);
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
        .filter(|worker| worker_online(worker, None, now_ms))
        .count();
    let active = active_session(path, store);
    let context = active.map(|session| {
        let folder = session.spawn_cwd.as_deref().unwrap_or(session.cwd.as_str());
        let title = workbench_title(path, session.custom_title.as_deref(), Some(folder));
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
            status: if worker_online(worker, None, now_ms) {
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
        coordinator: None,
        machine,
        agent,
        context,
        open_sessions,
        workers_online,
        workers_total,
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

/// The session a terminal path names, when it names one exactly.
///
/// `/t/:workerFp/*folderPath` and `/w/:workspaceId` do not name a session id.
/// Resolving those needs the folder index and its newest-wins tiebreak, which
/// belongs to the terminal surface; the chrome asks only the question it can
/// answer exactly, so the machine and agent items simply do not appear on a
/// folder route rather than naming a session the reader did not ask about.
fn active_session<'a>(path: &str, store: &'a roost_client_core::Store) -> Option<&'a Session> {
    if !is_terminal_route(path) {
        return None;
    }
    let wanted = path.strip_prefix("/s/")?;
    let wanted = wanted.split(['/', '?', '#']).next().unwrap_or(wanted);
    store
        .sessions
        .sessions()
        .values()
        .find(|session| session.id.as_str() == wanted)
}

/// Whether the document is in the foreground.
///
/// A backgrounded tab is not judged stale, so this is read at paint time rather
/// than tracked: a `visibilitychange` subscription with no coordinator health
/// snapshot to feed it would be a listener that never changes an answer.
#[cfg(target_arch = "wasm32")]
fn page_visible() -> bool {
    web_sys::window().is_some_and(|window| match window.document() {
        Some(document) => !document.hidden(),
        None => true,
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn page_visible() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use roost_client_core::ClientCore;

    fn bar(open: usize, online: usize, total: usize) -> StatusReadings {
        StatusReadings {
            coordinator: None,
            machine: None,
            agent: None,
            context: None,
            open_sessions: open,
            workers_online: online,
            workers_total: total,
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
                crate::components::design_icon::status_token(name).starts_with("--"),
                "{name} is not a status the dot styles a rule for"
            );
        }
    }

    #[test]
    fn the_coordinator_item_is_absent_rather_than_permanently_syncing() {
        // THE WHOLE POINT. With no health source the only words available are
        // `Syncing` forever or a fabricated model, and both are claims the
        // client cannot support. An absent item is distinguishable from a
        // healthy one; a permanent `Syncing` is not.
        let core = ClientCore::in_memory("tab-test");
        let readings = read_status("/", &Rc::new(RefCell::new(core)), 1_000);
        assert_eq!(readings.coordinator, None);
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
