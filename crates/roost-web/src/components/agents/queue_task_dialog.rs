//! The "Queue a task" modal: the shell's own dialog host for the task editor.
//! Ports `apps/web/src/components/agents/QueueTaskDialog.tsx`; mounted once by
//! `app::AuthorizedOverlays` beside the rename dialog and the palette, and
//! opened through `ShellIntent::OpenQueueTaskDialog` — which is how the
//! palette's "Queue task for this folder" row and every agent surface reach it.
//!
//! The open flag and its prefill live in the store, not here: the editor is
//! mounted from them or not at all, so there is no form to reset and no second
//! answer to "is it open".

use dioxus::prelude::*;
use libero::{
    components::Dialog,
    hooks::{ModalScope, use_modal},
};
use roost_client_core::ClientEvent;
use roost_client_core::store::shell_intent::ShellIntent;
use std::{cell::RefCell, rc::Rc};

use super::task_editor::TaskEditor;
use crate::components::overlay_generation::OverlayGeneration;
use crate::pump::use_store;

/// The dialog's headline.
pub const QUEUE_TASK_HEADLINE: &str = "Queue a task";

/// The host. Renders nothing at all while the store says the dialog is closed,
/// which is what takes `task-editor` off the page.
#[component]
pub fn QueueTaskDialogHost() -> Element {
    let pump = use_store();
    let stored = pump
        .core()
        .borrow()
        .store()
        .shell_dialogs
        .queue_task
        .clone();
    let request = stored.open.then(|| {
        (
            stored.prefill_body.clone(),
            stored.prefill_cwd.clone(),
            stored.prefill_worker_fp.clone(),
        )
    });
    let generation = use_hook(|| Rc::new(RefCell::new(OverlayGeneration::default())));
    let previous = use_hook(|| Rc::new(RefCell::new(None)));
    #[allow(clippy::type_complexity)]
    let modal = use_modal({
        let generation = Rc::clone(&generation);
        move |scope: ModalScope<((Option<String>, Option<String>, Option<String>), u64)>| {
            let Some(((default_body, default_cwd, default_worker_fp), token)) = scope.try_args()
            else {
                return rsx! {};
            };
            let generation = Rc::clone(&generation);
            let close_task = EventHandler::new(move |()| {
                if generation.borrow().is_current(token) {
                    scope.close();
                }
            });
            rsx! {
                Dialog { title: QUEUE_TASK_HEADLINE, close_button: Some(false),
                    TaskEditor {
                        default_body,
                        default_cwd,
                        default_worker_fp,
                        on_enqueued: Some(close_task),
                        on_cancel: Some(close_task),
                        show_cancel: true,
                    }
                }
            }
        }
    });
    let sync_modal = modal;
    let sync_pump = pump.clone();
    let sync_generation = Rc::clone(&generation);
    let sync_previous = Rc::clone(&previous);
    use_effect(use_reactive!(|(request)| {
        {
            let mut previous = sync_previous.borrow_mut();
            if *previous == request {
                return;
            }
            *previous = request.clone();
        }
        sync_generation.borrow_mut().invalidate();
        sync_modal.close();
        if let Some(request) = request {
            let token = sync_generation.borrow_mut().begin();
            let completion_generation = Rc::clone(&sync_generation);
            let completion_pump = sync_pump.clone();
            sync_modal.open_with((request, token)).onresult(move |_| {
                if completion_generation.borrow_mut().finish(token) {
                    completion_pump.dispatch(ClientEvent::Shell(ShellIntent::CloseQueueTaskDialog));
                }
            });
        }
    }));
    rsx! {}
}
