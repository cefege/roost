//! The browser-profile half of coding-agent notification: which ordered
//! transition is worth a card, and what each delivery acknowledges. Detection
//! itself is `roost_client_core::client::agents` — the status projection, the
//! completion policy and the per-profile acknowledgement ledger all live there.
//! This component only classifies the transitions the worker reports and raises
//! the cards. Ports `apps/web/src/components/notifications/AgentNotificationBridge.tsx`,
//! minus the one surface this build has no browser owner for: the service
//! worker's `roost-navigate` message. The title badge and the acknowledgement
//! live beside this in `agent_attention`, so this module holds one job.

pub mod agent_attention;
pub mod attention_count;

pub use agent_attention::AgentAttention;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::client::agents::AgentStatusRevisionToken;
use roost_client_core::client::agents::status_policy::{
    AgentStatusLevel, agent_status_completion_unseen, agent_status_revision_token,
    derive_agent_status_level,
};
use roost_client_core::store::selectors::session_by_id;
use roost_client_core::store::toasts::{ToastId, ToastKind, ToastOptions, ToastSource, add_toast};
use roost_protocol::wire::AgentStatus;

use super::store_write::write_store;
use crate::components::terminal::dom::{now_ms, page_visible};
use crate::platform::worker_paths::BrowserWorkerPaths;
use crate::pump::{Pump, use_store};
use crate::route_session::active_session_for_path;
use crate::router_state::use_location;
use crate::session_naming::session_title;

/// What a delivery is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentNotificationKind {
    /// The agent is waiting for a human.
    Blocked,
    /// The agent finished.
    Done,
}

impl AgentNotificationKind {
    /// The card's kind: a blocked agent is something to look at, a finished one
    /// is something that happened.
    const fn toast_kind(self) -> ToastKind {
        match self {
            Self::Blocked => ToastKind::Warn,
            Self::Done => ToastKind::Ok,
        }
    }

    /// The window, v2's own numbers.
    const fn ttl_ms(self) -> Option<u64> {
        match self {
            Self::Blocked => Some(8_000),
            Self::Done => Some(5_000),
        }
    }

    /// The verb the card's one line ends in.
    const fn verb(self) -> &'static str {
        match self {
            Self::Blocked => "needs your input",
            Self::Done => "finished",
        }
    }

    /// The log spelling.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::Done => "done",
        }
    }
}

/// What this profile last saw for a session, so the NEXT report reads as a
/// transition rather than as a level. Held here rather than in the store
/// because it is this profile's observation, not client state: a second tab
/// watching the same session has its own baseline and its own acknowledgements.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Baseline {
    level: AgentStatusLevel,
    token: AgentStatusRevisionToken,
}

/// Renders nothing itself. It exists for its effect, which is the one place a
/// delivery becomes a card.
#[component]
pub fn AgentNotifications() -> Element {
    let pump = use_store();
    // The RENDERED path, not a second one of its own. A `use_path_signal` here
    // was a fresh signal seeded from the address bar and a second `popstate`
    // listener that only `navigate_path`'s counterpart below the root could
    // move, so the dock decided which session this tab was looking at from a
    // path that froze at mount and drifted one navigation behind. The root
    // already provides the path both halves move together.
    let path = use_location();
    let baselines: Rc<RefCell<BTreeMap<String, Baseline>>> =
        use_hook(|| Rc::new(RefCell::new(BTreeMap::new())));

    // One read of the store per revision, and at most one card per transition
    // that survives every gate. Reading the revision here is what makes the
    // effect re-run when a status lands.
    use_effect(move || {
        let revision = pump.revision();
        let _ = revision.read();
        deliver(&pump, &path(), &mut baselines.borrow_mut());
    });

    rsx! {}
}

/// One report, already read against this profile's ledger.
struct Observed {
    session_id: String,
    level: AgentStatusLevel,
    token: AgentStatusRevisionToken,
    kind: Option<AgentNotificationKind>,
    message: Option<String>,
    title: String,
}

/// Compare every reported status against this profile's baseline and raise the
/// cards the transitions deserve.
fn deliver(pump: &Pump, path: &str, baselines: &mut BTreeMap<String, Baseline>) {
    for observed in observe(pump, path) {
        let previous = baselines.insert(
            observed.session_id.clone(),
            Baseline {
                level: observed.level,
                token: observed.token.clone(),
            },
        );
        // A baseline from a DIFFERENT OCCUPANT says nothing about this report:
        // a replacement agent numbers its own revisions from one, so the
        // previous agent's first report would look like a transition here. The
        // same occupant at a NEW REVISION is the case this exists for — that
        // is the one that has to raise the card, and comparing whole revision
        // tokens skipped it, so a blocked agent never blocked the toast.
        if previous
            .is_some_and(|previous| previous.token.identity_key() != observed.token.identity_key())
        {
            continue;
        }
        if let Some(kind) = observed.kind {
            raise(
                pump,
                &observed.session_id,
                kind,
                &observed.title,
                observed.message.as_deref(),
            );
        }
    }
}

/// Every status the store holds, read against the ledger and the path.
fn observe(pump: &Pump, path: &str) -> Vec<Observed> {
    let core = pump.core();
    let core = core.borrow();
    let store = core.store();
    let paths = BrowserWorkerPaths;
    let viewing = viewing_session_id(store, &paths, path);
    store
        .agent_status
        .statuses()
        .values()
        .map(|status| {
            let session_id = status.common.session_id.as_str();
            let acknowledged = store.agent_seen.acknowledged_revision(status);
            let level = derive_agent_status_level(Some(status), Some(acknowledged));
            Observed {
                session_id: session_id.to_owned(),
                level,
                token: agent_status_revision_token(status),
                kind: classify(status, level, acknowledged, viewing.as_deref()),
                message: status.common.message.clone(),
                title: session_by_id(store, session_id)
                    .map(|session| session_title(store, session))
                    .unwrap_or_else(|| "Terminal".to_owned()),
            }
        })
        .collect()
}

/// Which card, if any, this report earns.
fn classify(
    status: &AgentStatus,
    level: AgentStatusLevel,
    acknowledged: i64,
    viewing: Option<&str>,
) -> Option<AgentNotificationKind> {
    // A session on screen, in a tab that owns focus, is not a notification — it
    // is the thing the operator is looking at.
    if viewing == Some(status.common.session_id.as_str()) {
        return None;
    }
    match level {
        AgentStatusLevel::Blocked => Some(AgentNotificationKind::Blocked),
        // A completion this profile has not acknowledged: the same predicate the
        // unseen badge on a projected row is built from, so the card and the row
        // can never disagree about whether a completion was seen.
        _ if agent_status_completion_unseen(status, Some(acknowledged)) => {
            Some(AgentNotificationKind::Done)
        }
        _ => None,
    }
}

/// Raise the card, with the action that reveals the session and the window its
/// kind earns.
fn raise(
    pump: &Pump,
    session_id: &str,
    kind: AgentNotificationKind,
    title: &str,
    message: Option<&str>,
) {
    let id = ToastId::new(ToastSource::Host { name: "agent" }, session_id);
    let text = format!("{title} {}", kind.verb());
    let mut options = ToastOptions::with_ttl(kind.ttl_ms()).with_action("View", session_id);
    if let Some(message) = message
        && !message.trim().is_empty()
    {
        options = options.with_details(message);
    }
    let now = now_ms();
    write_store(pump, |store| {
        add_toast(store, id, text, kind.toast_kind(), options, now);
    });
    tracing::debug!(
        target: "notifications",
        session = session_id,
        outcome = kind.as_str(),
        "agent notification raised"
    );
}

/// The session this tab is actually looking at, and only while the tab is
/// visible AND focused: a Roost tab parked on a second monitor must keep its
/// blocked and done rows instead of silently marking them seen.
fn viewing_session_id(
    store: &roost_client_core::Store,
    paths: &BrowserWorkerPaths,
    path: &str,
) -> Option<String> {
    if !page_visible() {
        return None;
    }
    active_session_for_path(store, paths, path).map(|session| session.id.as_str().to_owned())
}
