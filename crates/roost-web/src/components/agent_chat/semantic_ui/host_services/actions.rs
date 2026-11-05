//! Browser effects for one OMP host-service prompt.
//!
//! Each attempt is admitted once per service: the store is rechecked before
//! anything happens, and the reply RPC is awaited before an in-app file open
//! navigates or a reserved tab receives its validated address.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::client::agent_chat::OmpHostService;

use crate::components::hooks::use_copy_clipboard::copy_text_then;
use crate::pump::Pump;

use super::super::links::{HostOpenTarget, resolve_host_open, resolve_stats_capability};
use super::browser::{absolute_document_url, open_blank_tab};
use super::controller::OmpHostServiceConfig;
use super::reply::{send_host_reply, service_is_pending};

fn begin_attempt(mut sending: Signal<BTreeSet<String>>, service_id: &str) -> bool {
    sending
        .try_write()
        .is_ok_and(|mut sending| sending.insert(service_id.to_owned()))
}

fn clear_sending(mut sending: Signal<BTreeSet<String>>, service_id: &str) {
    if let Ok(mut sending) = sending.try_write() {
        sending.remove(service_id);
    }
}

fn clear_failure(mut failures: Signal<BTreeMap<String, String>>, service_id: &str) {
    if let Ok(mut failures) = failures.try_write() {
        failures.remove(service_id);
    }
}

fn record_failure(
    pump: &Pump,
    service: &OmpHostService,
    mut failures: Signal<BTreeMap<String, String>>,
    message: String,
) {
    if !service_is_pending(pump, service) {
        return;
    }
    if let Ok(mut failures) = failures.try_write() {
        failures.insert(service.service_id.clone(), message);
    }
}

fn worker_os_of(pump: &Pump, worker_fp: &str) -> Option<String> {
    let core = pump.core();
    let core = core.borrow();
    crate::terminal_href::worker_os(core.store(), worker_fp).map(str::to_owned)
}

pub(super) fn attempt_copy(
    pump: Pump,
    service: OmpHostService,
    sending: Signal<BTreeSet<String>>,
    failures: Signal<BTreeMap<String, String>>,
) {
    if !service_is_pending(&pump, &service) || !begin_attempt(sending, &service.service_id) {
        return;
    }
    clear_failure(failures, &service.service_id);
    let text = service.value.clone();
    let settled = copy_completion(pump, service, sending, failures);
    copy_text_then(&text, move |accepted| settled.call(accepted));
}

fn copy_completion(
    pump: Pump,
    service: OmpHostService,
    sending: Signal<BTreeSet<String>>,
    failures: Signal<BTreeMap<String, String>>,
) -> EventHandler<bool> {
    let service_id = service.service_id.clone();
    let mut completion = Some((pump, service));
    EventHandler::new(move |accepted: bool| {
        let Some((reply_pump, service)) = completion.take() else {
            return;
        };
        if !accepted {
            clear_sending(sending, &service_id);
            record_failure(
                &reply_pump,
                &service,
                failures,
                "The browser refused clipboard access. Grant permission, then press Copy."
                    .to_owned(),
            );
            return;
        }
        let admitted_pump = reply_pump.clone();
        let admitted_service = service.clone();
        let admitted_id = service_id.clone();
        let sent = send_host_reply(
            reply_pump,
            service,
            true,
            None,
            Some(EventHandler::new(move |result: Result<(), String>| {
                clear_sending(sending, &admitted_id);
                if let Err(error) = result {
                    record_failure(
                        &admitted_pump,
                        &admitted_service,
                        failures,
                        format!("The text was copied, but OMP did not confirm it: {error}"),
                    );
                }
            })),
        );
        if !sent {
            clear_sending(sending, &service_id);
        }
    })
}

pub(super) fn attempt_open(
    pump: Pump,
    service: OmpHostService,
    config: OmpHostServiceConfig,
    navigate: EventHandler<String>,
    sending: Signal<BTreeSet<String>>,
    failures: Signal<BTreeMap<String, String>>,
) {
    if !service_is_pending(&pump, &service) || !begin_attempt(sending, &service.service_id) {
        return;
    }
    let service_id = service.service_id.clone();
    clear_failure(failures, &service_id);
    let worker_os = worker_os_of(&pump, &config.worker_fp);
    let target = match resolve_host_open(
        &service.value,
        &config.worker_fp,
        &config.cwd,
        worker_os.as_deref(),
    ) {
        Ok(target) => target,
        Err(error) => {
            clear_sending(sending, &service_id);
            record_failure(&pump, &service, failures, error);
            return;
        }
    };
    match target {
        HostOpenTarget::Browser(href) => {
            let tab = match open_blank_tab() {
                Ok(tab) => Rc::new(RefCell::new(Some(tab))),
                Err(error) => {
                    clear_sending(sending, &service_id);
                    record_failure(&pump, &service, failures, error);
                    return;
                }
            };
            let admitted_pump = pump.clone();
            let admitted_service = service.clone();
            let admitted_id = service_id.clone();
            let admitted_tab = Rc::clone(&tab);
            let sent = send_host_reply(
                pump,
                service,
                true,
                None,
                Some(EventHandler::new(move |result: Result<(), String>| {
                    clear_sending(sending, &admitted_id);
                    let Some(tab) = admitted_tab.borrow_mut().take() else {
                        return;
                    };
                    match result {
                        Ok(()) => {
                            if let Err(error) = tab.navigate(&href) {
                                tab.close();
                                tracing::warn!(
                                    target: "agent_chat",
                                    service_id = %admitted_id,
                                    "OMP host-service tab navigation failed: {error}"
                                );
                            }
                        }
                        Err(error) => {
                            tab.close();
                            record_failure(
                                &admitted_pump,
                                &admitted_service,
                                failures,
                                format!(
                                    "OMP did not confirm the link, so the new tab was closed: {error}"
                                ),
                            );
                        }
                    }
                })),
            );
            if !sent {
                if let Some(tab) = tab.borrow_mut().take() {
                    tab.close();
                }
                clear_sending(sending, &service_id);
            }
        }
        HostOpenTarget::WorkerFile(href) => {
            let admitted_pump = pump.clone();
            let admitted_service = service.clone();
            let admitted_id = service_id.clone();
            let sent = send_host_reply(
                pump,
                service,
                true,
                None,
                Some(EventHandler::new(move |result: Result<(), String>| {
                    clear_sending(sending, &admitted_id);
                    match result {
                        Ok(()) => navigate.call(href.clone()),
                        Err(error) => record_failure(
                            &admitted_pump,
                            &admitted_service,
                            failures,
                            format!(
                                "OMP did not confirm the file open, so it was not opened: {error}"
                            ),
                        ),
                    }
                })),
            );
            if !sent {
                clear_sending(sending, &service_id);
            }
        }
    }
}

pub(super) fn attempt_stats(
    pump: Pump,
    service: OmpHostService,
    sending: Signal<BTreeSet<String>>,
    failures: Signal<BTreeMap<String, String>>,
) {
    if !service_is_pending(&pump, &service) || !begin_attempt(sending, &service.service_id) {
        return;
    }
    let service_id = service.service_id.clone();
    clear_failure(failures, &service_id);
    let href = match resolve_stats_capability(&service.value)
        .and_then(|path| absolute_document_url(&path))
    {
        Ok(href) => href,
        Err(error) => {
            clear_sending(sending, &service_id);
            record_failure(&pump, &service, failures, error);
            return;
        }
    };
    let tab = match open_blank_tab() {
        Ok(tab) => Rc::new(RefCell::new(Some(tab))),
        Err(error) => {
            clear_sending(sending, &service_id);
            record_failure(&pump, &service, failures, error);
            return;
        }
    };
    let admitted_pump = pump.clone();
    let admitted_service = service.clone();
    let admitted_id = service_id.clone();
    let admitted_tab = Rc::clone(&tab);
    let sent = send_host_reply(
        pump,
        service,
        true,
        None,
        Some(EventHandler::new(move |result: Result<(), String>| {
            clear_sending(sending, &admitted_id);
            let Some(tab) = admitted_tab.borrow_mut().take() else {
                return;
            };
            match result {
                Ok(()) => {
                    if let Err(error) = tab.navigate(&href) {
                        tab.close();
                        tracing::warn!(
                            target: "agent_chat",
                            service_id = %admitted_id,
                            "OMP stats tab navigation failed: {error}"
                        );
                    }
                }
                Err(error) => {
                    tab.close();
                    record_failure(
                        &admitted_pump,
                        &admitted_service,
                        failures,
                        format!(
                            "OMP did not confirm the dashboard request, so the tab was closed: {error}"
                        ),
                    );
                }
            }
        })),
    );
    if !sent {
        if let Some(tab) = tab.borrow_mut().take() {
            tab.close();
        }
        clear_sending(sending, &service_id);
    }
}

pub(super) fn dismiss_service(
    pump: Pump,
    service: OmpHostService,
    failures: Signal<BTreeMap<String, String>>,
) {
    let admitted_pump = pump.clone();
    let admitted_service = service.clone();
    send_host_reply(
        pump,
        service,
        false,
        Some("The operator dismissed this request.".to_owned()),
        Some(EventHandler::new(move |result: Result<(), String>| {
            if let Err(error) = result {
                record_failure(
                    &admitted_pump,
                    &admitted_service,
                    failures,
                    format!("OMP did not receive the dismissal: {error}"),
                );
            }
        })),
    );
}

#[cfg(test)]
#[path = "copy_tests.rs"]
mod copy_tests;
