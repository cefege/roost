//! Places an authenticated child conversation in the existing native HTML deck.
//! A browser window is reserved under a gesture; pane splits use normal DeckIntent.
//! The source is acknowledged only after the actual child editor is mounted.

use super::{
    actions::{begin_attempt, clear_failure, clear_sending, record_failure},
    browser::open_blank_tab,
    reply::service_is_pending,
};
use crate::pump::Pump;
use dioxus::prelude::*;
use roost_client_core::client::agent_chat::OmpHostService;
use roost_protocol::native_fork::{ForkPlacement, ForkTarget};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn attempt(
    pump: Pump,
    service: OmpHostService,
    sending: Signal<BTreeSet<String>>,
    failures: Signal<BTreeMap<String, String>>,
) {
    if !service_is_pending(&pump, &service) || !begin_attempt(sending, &service.service_id) {
        return;
    }
    clear_failure(failures, &service.service_id);
    let Some(target) = ForkTarget::parse(&service.value)
        .filter(|target| target.parent_conversation_id == service.conversation_id)
    else {
        clear_sending(sending, &service.service_id);
        record_failure(
            &pump,
            &service,
            failures,
            "The fork placement binding is invalid.".into(),
        );
        return;
    };
    let tab = if target.placement == ForkPlacement::Window {
        match open_blank_tab() {
            Ok(tab) => Some(tab),
            Err(error) => {
                clear_sending(sending, &service.service_id);
                record_failure(
                    &pump,
                    &service,
                    failures,
                    format!("{error} The fork is awaiting Open fork; it has not opened."),
                );
                return;
            }
        }
    } else {
        None
    };
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(web::place(pump, service, target, tab, sending, failures));
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (target, tab);
        clear_sending(sending, &service.service_id);
        record_failure(
            &pump,
            &service,
            failures,
            "Fork placement requires a browser.".into(),
        );
    }
}

#[cfg(target_arch = "wasm32")]
mod web {
    use super::super::{
        browser::{PendingTab, absolute_document_url},
        reply::send_host_reply,
    };
    use super::*;
    use crate::{
        components::{deck::terminal_deck_model::deck_folder_for_agent, terminal::dom::sleep_ms},
        platform::worker_paths::BrowserWorkerPaths,
    };
    use roost_client_core::{
        ClientEvent,
        deck::{DeckIntent, agent_tab_id},
    };
    use roost_protocol::layout::document::LayoutDirection;
    use wasm_bindgen::JsCast;

    pub(super) async fn place(
        pump: Pump,
        service: OmpHostService,
        target: ForkTarget,
        tab: Option<PendingTab>,
        sending: Signal<BTreeSet<String>>,
        failures: Signal<BTreeMap<String, String>>,
    ) {
        let result = place_child(&pump, &service, &target, tab.as_ref()).await;
        if let Err(error) = result {
            if let Some(tab) = tab {
                tab.close();
            }
            clear_sending(sending, &service.service_id);
            record_failure(&pump, &service, failures, error);
            return;
        }
        if !service_is_pending(&pump, &service) {
            if let Some(tab) = tab {
                tab.close();
            }
            clear_sending(sending, &service.service_id);
            return;
        }
        let reply_pump = pump.clone();
        let reply_service = service.clone();
        let admitted = EventHandler::new(move |result: Result<(), String>| {
            clear_sending(sending, &reply_service.service_id);
            match result {
                Ok(()) => {
                    let document = tab
                        .as_ref()
                        .and_then(PendingTab::document)
                        .or_else(|| web_sys::window().and_then(|window| window.document()));
                    if let Some(document) = document {
                        focus_child(&document, &target.conversation_id);
                    }
                }
                Err(error) => {
                    if let Some(tab) = &tab {
                        tab.close();
                    }
                    record_failure(&reply_pump, &reply_service, failures, error);
                }
            }
        });
        send_host_reply(pump, service, true, None, None, Some(admitted));
    }

    async fn place_child(
        pump: &Pump,
        service: &OmpHostService,
        target: &ForkTarget,
        tab: Option<&PendingTab>,
    ) -> Result<(), String> {
        let document = web_sys::window()
            .and_then(|window| window.document())
            .ok_or("The parent browser view is unavailable.")?;
        let mut placed = false;
        for _ in 0..400 {
            if !service_is_pending(pump, service) {
                return Err("The fork request was cancelled.".into());
            }
            if !placed {
                let row = pump
                    .core()
                    .borrow()
                    .store()
                    .agent_chat
                    .conversations
                    .get(&target.conversation_id)
                    .cloned();
                if let Some(child) = row {
                    if child.parent_id.as_deref() != Some(&target.parent_conversation_id) {
                        return Err("The child does not belong to this parent.".into());
                    }
                    match target.placement {
                        ForkPlacement::Window => {
                            let href =
                                absolute_document_url(&format!("/a/{}", target.conversation_id))?;
                            tab.ok_or("The child browser window is unavailable.")?
                                .navigate(&href)?;
                        }
                        ForkPlacement::Pane => {
                            let parent_selector = format!(
                                "[data-omp-conversation='{}']",
                                target.parent_conversation_id
                            );
                            let pane_id = document
                                .query_selector(&parent_selector)
                                .ok()
                                .flatten()
                                .and_then(|element| element.get_attribute("data-pane-id"))
                                .filter(|id| !id.is_empty())
                                .ok_or("The parent pane is no longer mounted.")?;
                            let folder = {
                                let core = pump.core();
                                let core = core.borrow();
                                deck_folder_for_agent(core.store(), &BrowserWorkerPaths, &child)
                            };
                            pump.dispatch(ClientEvent::Deck(DeckIntent::SplitPane {
                                folder,
                                pane_id,
                                direction: LayoutDirection::Row,
                                tab_id: agent_tab_id(&target.conversation_id),
                                insert_first: false,
                            }));
                        }
                    }
                    placed = true;
                }
            }
            if placed {
                let child_document = match tab {
                    Some(tab) => tab.document(),
                    None => Some(document.clone()),
                };
                if child_document.as_ref().is_some_and(|document| {
                    child_editor(document, &target.conversation_id).is_some()
                }) {
                    tracing::info!(parent_conversation_id=%target.parent_conversation_id, conversation_id=%target.conversation_id, "OMP child editor placed in native HTML");
                    return Ok(());
                }
            }
            sleep_ms(50).await;
        }
        Err("The fork child did not become interactive; no opened acknowledgment was sent.".into())
    }

    fn child_editor(document: &web_sys::Document, id: &str) -> Option<web_sys::HtmlElement> {
        let selector =
            format!("[data-omp-conversation='{id}'] [data-omp-surface] textarea:not([readonly])");
        let element = document.query_selector(&selector).ok().flatten()?;
        let bounds = element.get_bounding_client_rect();
        (bounds.width() > 0.0 && bounds.height() > 0.0)
            .then(|| element.dyn_into().ok())
            .flatten()
    }

    fn focus_child(document: &web_sys::Document, id: &str) {
        if let Some(editor) = child_editor(document, id) {
            let _ = editor.focus();
        }
    }
}
