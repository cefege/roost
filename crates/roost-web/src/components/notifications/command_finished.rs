//! "Command finished" toasts: a shell command that ran for at least 10 s
//! (OSC 133, measured by the worker) and ended while nobody in this tab was
//! looking at its session. Mounted by `NotificationDock`; drains
//! `Store::command_finished_requests` after a 1 s delay, honoring attention
//! and the configured completion sound.

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::store::command_finished_requests::CommandFinishedRequest;
use roost_client_core::store::prefs::notify::NotifyPref;
use roost_client_core::store::selectors::session_by_id;
use roost_client_core::store::toasts::{ToastId, ToastKind, ToastOptions, ToastSource, add_toast};

use super::command_finished_scheduler::{
    ArmedCommandFinished, COMMAND_FINISHED_DELAY_MS, CommandFinishedScheduler,
};
use super::notification_tone::{DONE_CUE, TonePlayer};
use super::page_attention;
use super::store_write::write_store;
use super::terminal_notification::has_live_terminal_notification;
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
    let attention = page_attention::use_page_attention();
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
            let armed = watch.borrow_mut().scheduler.observe(
                &request.session_id,
                viewing.as_deref() == Some(request.session_id.as_str()),
            );
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
        let viewing = page_attention::attended_now()
            .then(|| active_session_for_path(store, &BrowserWorkerPaths, path))
            .flatten()
            .map(|session| session.id.as_str().to_owned());
        if viewing.as_deref() == Some(request.session_id.as_str())
            || !store.prefs.notify.get(NotifyPref::CommandFinished)
        {
            None
        } else {
            session_by_id(store, &request.session_id).map(|session| {
                (
                    session_title(store, session),
                    store.prefs.notify.get(NotifyPref::InApp),
                    store.prefs.notify.get(NotifyPref::DoneSound),
                    has_live_terminal_notification(&store.toasts, &request.session_id),
                )
            })
        }
    };
    let Some((title, show_toast, sound, superseded)) = alert else {
        return;
    };
    if show_toast {
        match request.exit_code {
            // The program already told the reader this session is done, so a
            // second success card would only repeat it. A failure stays: the
            // exit code is information OSC 9 does not carry.
            Some(0) | None => {
                if !superseded {
                    raise_success(pump, request, &title);
                }
            }
            Some(code) => raise_failure(pump, request, &title, code),
        }
    }
    if sound {
        tones.play_notes(&DONE_CUE);
    }
    tracing::debug!(
        target: "notifications",
        session_id = %request.session_id,
        delivery_seq = request.delivery_seq,
        ticket = armed.ticket,
        superseded,
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

/// `2m 13s`, `1h 4m`, `45s`: the scale a reader compares runs at.
fn format_duration(duration_ms: u64) -> String {
    let seconds = duration_ms / 1_000;
    match (seconds / 3_600, seconds / 60 % 60, seconds % 60) {
        (0, 0, seconds) => format!("{seconds}s"),
        (0, minutes, seconds) => format!("{minutes}m {seconds}s"),
        (hours, minutes, _) => format!("{hours}h {minutes}m"),
    }
}
