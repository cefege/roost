//! The desktop status bar projects coordinator-link health, active-session
//! context and fleet counts from one store reading. AppShell mounts it;
//! client-core owns transport evidence and agent-status policy.

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
use crate::platform::visibility::page_visible;
use libero::{
    components::{Box, Flex, Indicator},
    sx::sx,
};

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
    let bar_sx = use_hook(|| {
        sx().display("flex")
            .min_width("0")
            .align_items("center")
            .gap("md")
            .overflow("hidden")
            .border_top("1px solid var(--lsx-paper-border-color)")
            .background("var(--workbench-status)")
            .padding_inline("sm")
            .font_weight("500")
            .font_size("xs")
            .color("muted")
            .with("grid-area", "statusbar")
    });
    let group_sx = use_hook(|| sx().display("contents"));
    let item_sx = use_hook(|| {
        sx().display("inline-flex")
            .min_width("0")
            .flex_shrink("0")
            .align_items("center")
            .gap("xs")
            .white_space("nowrap")
            .with("font-variant-numeric", "tabular-nums")
    });
    let narrow_item_sx = use_hook(|| {
        item_sx
            .clone()
            .media("(max-width: 960px)", sx().display("none"))
    });
    let context_sx = use_hook(|| {
        item_sx
            .clone()
            .overflow("hidden")
            .with("overscroll-behavior-y", "auto")
            .text_overflow("ellipsis")
            .color("muted")
            .media("(max-width: 1100px)", sx().display("none"))
    });
    let count_sx = use_hook(|| item_sx.clone());
    let revision_sx = use_hook(|| {
        item_sx
            .clone()
            .margin_left("auto")
            .font_family("var(--font-mono)")
            .color("muted")
            .media("(max-width: 1100px)", sx().display("none"))
    });
    rsx! {
        Box { component: "footer", sx: bar_sx,
            "data-testid": "workbench-status-bar",
            "aria-label": "Workbench status",
            Box { sx: group_sx.clone(),
                Flex { direction: "row", wrap: false, sx: item_sx,
                    "data-testid": "workbench-status-sync",
                    Indicator { color: status_color(coordinator.status()), size: "xs" }
                    Box { component: "span", {coordinator.label()} }
                }
                if let Some(reading) = machine {
                    Flex { direction: "row", wrap: false, sx: narrow_item_sx.clone(),
                        "data-testid": "workbench-status-worker",
                        Indicator { color: status_color(&reading.status), size: "xs" }
                        Box { component: "span", {reading.label} }
                    }
                }
                if let Some(reading) = agent {
                    Flex { direction: "row", wrap: false, sx: narrow_item_sx.clone(),
                        "data-testid": "workbench-status-agent",
                        Indicator { color: status_color(&reading.status), size: "xs" }
                        Box { component: "span", {reading.label} }
                    }
                }
                if let Some(context) = context {
                    Flex { direction: "row", wrap: false, sx: context_sx,
                        "data-testid": "workbench-status-context",
                        {context}
                    }
                }
                if let Some(session_id) = focused_session {
                    crate::components::terminal_signal_marks::TerminalUserVarBadges { session_id }
                }
            }
            Box { sx: group_sx.clone(),
                Flex { direction: "row", wrap: false, sx: count_sx,
                    "data-testid": "workbench-status-counts",
                    {readings.sessions_worded()}
                    Box { component: "span", "aria-hidden": "true", "·" }
                    {readings.workers_worded()}
                }
                if let Some(revision) = readings.revision.clone() {
                    Flex { direction: "row", wrap: false, sx: revision_sx,
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
fn status_color(status: &str) -> &'static str {
    match status {
        "ok" => "success",
        "warn" | "offline" => "warning",
        "info" => "info",
        "idle" => "muted",
        _ => "muted",
    }
}

/// The design-system status an agent level's dot shows.
///
/// The core's `AgentDotStatus` is a four-member vocabulary of its own; this is
/// the one place it is spelled as the status names the bar shares with sidebar chips.
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
