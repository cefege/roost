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
use roost_client_core::ClientEvent;
use roost_client_core::store::shell_intent::ShellIntent;

use super::task_editor::TaskEditor;
use crate::components::md::Dialog;
use crate::pump::use_store;

/// The dialog's headline.
pub const QUEUE_TASK_HEADLINE: &str = "Queue a task";

/// The host. Renders nothing at all while the store says the dialog is closed,
/// which is what takes `task-editor` off the page.
#[component]
pub fn QueueTaskDialogHost() -> Element {
    let pump = use_store();
    let dialog = pump
        .core()
        .borrow()
        .store()
        .shell_dialogs
        .queue_task
        .clone();
    if !dialog.open {
        return rsx! {};
    }
    let close = {
        let pump = pump.clone();
        EventHandler::new(move |()| {
            pump.dispatch(ClientEvent::Shell(ShellIntent::CloseQueueTaskDialog));
        })
    };
    // Queued and cancelled are the same answer for this dialog: the task is not
    // on the queue and the editor is empty again, which is what unmounting it
    // means. A second path that only toasted would leave the form mounted over a
    // queue the reader can no longer see.
    let enqueued = close;
    let cancelled = close;
    rsx! {
        Dialog {
            open: true,
            on_close: close,
            headline: Some(QUEUE_TASK_HEADLINE.to_owned()),
            // The editor's own band carries Cancel and Queue, so a close button
            // above them would be a third way out of a form with two answers.
            show_close_button: Some(false),
            TaskEditor {
                default_body: dialog.prefill_body,
                default_cwd: dialog.prefill_cwd,
                default_worker_fp: dialog.prefill_worker_fp,
                on_enqueued: Some(enqueued),
                on_cancel: Some(cancelled),
                show_cancel: true,
            }
        }
    }
}
