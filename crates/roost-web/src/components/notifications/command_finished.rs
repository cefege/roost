//! "Command finished" toasts: a shell command that ran for at least 10 s
//! (OSC 133, measured by the worker) and ended while nobody in this tab was
//! looking at its session. Mounted by `NotificationDock`; drains
//! `Store::command_finished_requests` and delivers through the same 1 s delay,
//! attention rules and "Sound when finished" cue as the agent notifications.
//! A session with an agent status is skipped: the agent's own done toast says it.

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::store::command_finished_requests::CommandFinishedRequest;
use roost_client_core::store::prefs::notify::NotifyPref;
use roost_client_core::store::selectors::session_by_id;
use roost_client_core::store::toasts::{ToastId, ToastKind, ToastOptions, ToastSource, add_toast};

use super::agent_notifications::agent_attention;
use super::agent_notifications::notification_tone::TonePlayer;
use super::agent_notifications::scheduler::AgentNotificationKind;
use super::command_finished_scheduler::{
    ArmedCommandFinished, COMMAND_FINISHED_DELAY_MS, CommandFinishedScheduler,
};
use super::store_write::write_store;
use crate::components::terminal::dom::{now_ms, sleep_ms};
use crate::platform::worker_paths::BrowserWorkerPaths;
use crate::pump::{Pump, use_store};
use crate::route_session::active_session_for_path;
use crate::router_state::use_location;
use crate::session_naming::session_title;

#[derive(Debug, Default)]
struct Watch {
    scheduler: CommandFinishedScheduler,
}

/// Drains command completions and schedules the cards this tab is entitled to show.
#[component]
pub fn CommandFinished() -> Element {
    let pump = use_store();
    let path = use_location();
    let attention = agent_attention::use_page_attention();
    let watch = use_hook(|| Rc::new(RefCell::new(Watch::default())));
    let tones = use_hook(|| Rc::new(TonePlayer::default()));
    use_effect(move || {
        let _ = pump.revision().read();
        let attended = *attention.read();
        let requests = {
            let core = pump.core();
            let mut core = core.borrow_mut();
            core.store_mut()
                .command_finished_requests
                .drain()
                .collect::<Vec<_>>()
        };
        if requests.is_empty() {
            return;
        }
        let viewing = if attended {
            let core = pump.core();
            let core = core.borrow();
            active_session_for_path(core.store(), &BrowserWorkerPaths, &path())
                .map(|session| session.id.as_str().to_owned())
        } else {
            None
        };
        for request in requests {
            let armed = {
                let core = pump.core();
                let core = core.borrow();
                let status_exists = has_agent_status(core.store(), &request.session_id);
                watch.borrow_mut().scheduler.observe(
                    &request.session_id,
                    status_exists || viewing.as_deref() == Some(request.session_id.as_str()),
                )
            };
            if let Some(armed) = armed {
                let pump = pump.clone();
                let watch = Rc::clone(&watch);
                let tones = Rc::clone(&tones);
                spawn(async move {
                    sleep_ms(COMMAND_FINISHED_DELAY_MS).await;
                    if watch.borrow_mut().scheduler.take_due(&armed) {
                        deliver(&pump, &tones, &path.peek(), &request, &armed);
                    }
                });
            }
        }
    });
    rsx! {}
}

fn deliver(
    pump: &Pump,
    tones: &TonePlayer,
    path: &str,
    request: &CommandFinishedRequest,
    armed: &ArmedCommandFinished,
) {
    let alert = {
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        let viewing = agent_attention::attended_now()
            .then(|| active_session_for_path(store, &BrowserWorkerPaths, path))
            .flatten()
            .map(|session| session.id.as_str().to_owned());
        if viewing.as_deref() == Some(request.session_id.as_str())
            || has_agent_status(store, &request.session_id)
            || !store.prefs.notify.get(NotifyPref::CommandFinished)
        {
            None
        } else {
            session_by_id(store, &request.session_id).map(|session| {
                (
                    session_title(store, session),
                    store.prefs.notify.get(NotifyPref::InApp),
                    store.prefs.notify.get(NotifyPref::DoneSound),
                )
            })
        }
    };
    let Some((title, show_toast, sound)) = alert else {
        return;
    };
    if show_toast {
        match request.exit_code {
            Some(0) | None => raise_success(pump, request, &title),
            Some(code) => raise_failure(pump, request, &title, code),
        }
    }
    if sound {
        tones.play(AgentNotificationKind::Done);
    }
    tracing::debug!(
        target: "notifications",
        session_id = %request.session_id,
        delivery_seq = request.delivery_seq,
        ticket = armed.ticket,
        "command completion notification delivered"
    );
}

fn raise_success(pump: &Pump, request: &CommandFinishedRequest, title: &str) {
    let id = ToastId::new(
        ToastSource::Host {
            name: "command-finished",
        },
        &request.session_id,
    );
    let text = format!(
        "{title} finished · {}",
        format_duration(request.duration_ms)
    );
    let options = ToastOptions::with_ttl(Some(5_000)).with_action("View", &request.session_id);
    write_store(pump, |store| {
        add_toast(store, id, text, ToastKind::Ok, options, now_ms())
    });
}

fn raise_failure(pump: &Pump, request: &CommandFinishedRequest, title: &str, code: i32) {
    let id = ToastId::new(
        ToastSource::Host {
            name: "command-finished",
        },
        &request.session_id,
    );
    let text = format!(
        "{title} failed · exit {code} · {}",
        format_duration(request.duration_ms)
    );
    let options = ToastOptions::with_ttl(Some(8_000)).with_action("View", &request.session_id);
    write_store(pump, |store| {
        add_toast(store, id, text, ToastKind::Err, options, now_ms())
    });
}

/// Whether the session has an agent status: the agent's own done toast already
/// covers a command that is an agent run.
fn has_agent_status(store: &roost_client_core::store::Store, session_id: &str) -> bool {
    roost_protocol::wire::SessionId::try_from(session_id)
        .is_ok_and(|session| store.agent_status.status(&session).is_some())
}

/// `2m 13s`, `1h 4m`, `45s`: the scale a reader compares runs at.
fn format_duration(duration_ms: u64) -> String {
    let seconds = duration_ms / 1_000;
    match (seconds / 3_600, seconds / 60 % 60, seconds % 60) {
        (0, 0, seconds) => format!("{seconds}s"),
        (0, minutes, seconds) => format!("{minutes}m {seconds}s"),
        (hours, minutes, _) => format!("{hours}h {minutes}m"),
    }
}
