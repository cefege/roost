//! The browser-profile half of coding-agent notification: watch every status
//! the store holds, hand each observed CHANGE to the transition scheduler, and
//! raise the card a due delivery still earns. Detection itself is
//! `roost_client_core::client::agents`; the transition rules are `scheduler`.
//! Ports `apps/web/src/components/notifications/AgentNotificationBridge.tsx`,
//! minus the surfaces this build has no browser owner for: the service worker's
//! `roost-navigate` message, the cross-tab delivery claim, and the sound cue.
//! The title badge and the acknowledgement live beside this in `agent_attention`.

pub mod agent_attention;
pub mod attention_count;
pub mod scheduler;

pub use agent_attention::AgentAttention;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::Store;
use roost_client_core::store::prefs::notify::NotifyPref;
use roost_client_core::store::selectors::session_by_id;
use roost_client_core::store::toasts::{ToastId, ToastKind, ToastOptions, ToastSource, add_toast};
use roost_protocol::wire::{AgentStatus, SessionId};

use self::scheduler::{
    AGENT_NOTIFICATION_DELAY_MS, AgentNotificationDelivery, AgentNotificationKind,
    AgentNotificationScheduler, ArmedNotification, matches_agent_notification,
};
use super::store_write::write_store;
use crate::components::terminal::dom::{now_ms, sleep_ms};
use crate::platform::worker_paths::BrowserWorkerPaths;
use crate::pump::{Pump, use_store};
use crate::route_session::active_session_for_path;
use crate::router_state::use_location;
use crate::session_naming::session_title;

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

/// What this profile last observed per session, and the deliveries waiting on
/// a timer. Held here rather than in the store because it is this profile's
/// observation, not client state: a second tab has its own.
#[derive(Debug, Default)]
struct NotificationWatch {
    observed: BTreeMap<SessionId, AgentStatus>,
    scheduler: AgentNotificationScheduler,
}

/// Renders nothing itself. It exists for its effect, which is the one place a
/// status change becomes a pending card, and its timers, the one place a
/// pending card is raised.
#[component]
pub fn AgentNotifications() -> Element {
    let pump = use_store();
    // The RENDERED path, so the session this tab is looking at moves with the
    // same navigation that moves the page.
    let path = use_location();
    let watch = use_hook(|| Rc::new(RefCell::new(NotificationWatch::default())));

    // Reading the revision is what re-runs this on every store write, including
    // the card this component raises; only a status that CHANGED since the last
    // run reaches the scheduler, so that write raises nothing further.
    use_effect(move || {
        let revision = pump.revision();
        let _ = revision.read();
        let armed = observe_changes(&pump, &path(), &mut watch.borrow_mut());
        for armed in armed {
            let pump = pump.clone();
            let watch = Rc::clone(&watch);
            spawn(async move {
                sleep_ms(AGENT_NOTIFICATION_DELAY_MS).await;
                let due = watch.borrow_mut().scheduler.take_due(&armed);
                if let Some(delivery) = due {
                    deliver(&pump, &path.peek(), &delivery);
                }
            });
        }
    });

    rsx! {}
}

/// Hand every status that changed since the last observation to the
/// scheduler, and return the timers it armed.
fn observe_changes(
    pump: &Pump,
    path: &str,
    watch: &mut NotificationWatch,
) -> Vec<ArmedNotification> {
    let core = pump.core();
    let core = core.borrow();
    let store = core.store();
    let statuses = store.agent_status.statuses();
    let viewing = viewing_session_id(store, path);
    let NotificationWatch {
        observed,
        scheduler,
    } = watch;

    observed.retain(|session_id, _| {
        let present = statuses.contains_key(session_id);
        if !present {
            scheduler.cancel(session_id.as_str());
        }
        present
    });
    let mut armed = Vec::new();
    for (session_id, status) in statuses {
        let previous = observed.insert(session_id.clone(), status.clone());
        if previous.as_ref() == Some(status) {
            continue;
        }
        let viewed = viewing.as_deref() == Some(session_id.as_str());
        armed.extend(scheduler.observe(
            session_id.as_str(),
            previous.as_ref(),
            Some(status),
            viewed,
        ));
    }
    // A session the reader just opened owes no card for what it is showing;
    // `agent_attention` spends its acknowledgement.
    if let Some(viewed) = viewing {
        scheduler.cancel(&viewed);
    }
    armed
}

/// Raise the card a due delivery earns, when it still earns one.
fn deliver(pump: &Pump, path: &str, delivery: &AgentNotificationDelivery) {
    let card = {
        let core = pump.core();
        let core = core.borrow();
        card_for(core.store(), path, delivery)
    };
    if let Some((title, message)) = card {
        raise(pump, delivery, &title, message.as_deref());
    }
}

/// The card's title and details, or `None` when the agent moved on, the reader
/// is looking at the session, this profile already acknowledged the revision,
/// or in-app cards are off.
fn card_for(
    store: &Store,
    path: &str,
    delivery: &AgentNotificationDelivery,
) -> Option<(String, Option<String>)> {
    let status = store.agent_status.status(&delivery.token.session_id);
    if !matches_agent_notification(status, delivery) {
        return None;
    }
    let status = status?;
    if viewing_session_id(store, path).as_deref() == Some(delivery.session_id.as_str())
        || store.agent_seen.acknowledged_revision(status) >= delivery.token.revision
        || !store.prefs.notify.get(NotifyPref::InApp)
    {
        return None;
    }
    let title = session_by_id(store, &delivery.session_id)
        .map(|session| session_title(store, session))
        .unwrap_or_else(|| "Terminal".to_owned());
    Some((title, status.common.message.clone()))
}

/// Raise the card, with the action that reveals the session and the window its
/// kind earns.
fn raise(pump: &Pump, delivery: &AgentNotificationDelivery, title: &str, message: Option<&str>) {
    let session_id = delivery.session_id.as_str();
    let kind = delivery.kind;
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
/// visible AND its window owns focus: a Roost tab parked on a second monitor
/// still gets the card.
fn viewing_session_id(store: &Store, path: &str) -> Option<String> {
    if !agent_attention::attended_now() {
        return None;
    }
    active_session_for_path(store, &BrowserWorkerPaths, path)
        .map(|session| session.id.as_str().to_owned())
}
