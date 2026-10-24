//! The rename dialog host for a session's or a folder's context-menu action:
//! the shared `Dialog` owns focus and dismissal; the store supplies the one
//! active request (`store.shell_dialogs.rename`); the commit renames the session
//! (`SessionsRename`, empty clears it) or names the folder's workspace (update
//! when one exists, else create). Ports `apps/web/src/components/RenameDialog.tsx`;
//! mounted once by `app::AuthorizedOverlays`; opened by the sidebar row menus
//! through `ShellIntent::OpenRenameDialog`.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::store::shell_dialogs::{RenameDialogRequest, RenameTarget};
use roost_client_core::store::shell_intent::ShellIntent;

use libero::{
    components::{Button, Dialog, Flex, Text, TextField},
    hooks::{ModalScope, use_modal},
    sx::sx,
};
use std::{cell::RefCell, rc::Rc};

use crate::components::overlay_generation::OverlayGeneration;
use crate::pump::{Pump, use_store};

/// The dialog's default title.
pub const RENAME_TERMINAL_HEADLINE: &str = "Rename terminal";

/// The name a commit sends: trimmed, as v2 sent it.
pub fn committed_name(input: &str) -> String {
    input.trim().to_owned()
}

/// The host.
#[component]
pub fn RenameDialogHost() -> Element {
    let pump = use_store();
    let request = pump.core().borrow().store().shell_dialogs.rename.clone();
    let mut name = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut seeded = use_signal(|| None::<RenameDialogRequest>);
    if *seeded.peek() != request {
        if let Some(open) = request.as_ref() {
            name.set(open.current_title.clone());
            busy.set(false);
        }
        seeded.set(request.clone());
    }
    let generation = use_hook(|| Rc::new(RefCell::new(OverlayGeneration::default())));
    let previous = use_hook(|| Rc::new(RefCell::new(None::<RenameDialogRequest>)));
    let field_sx = use_hook(|| sx().width("100%"));
    let footer_sx = use_hook(|| sx().align_items("center").gap("sm"));
    let modal = use_modal({
        let pump = pump.clone();
        let generation = Rc::clone(&generation);
        move |scope: ModalScope<(RenameDialogRequest, u64)>| {
            let Some((open, token)) = scope.try_args() else {
                return rsx! {};
            };
            let target = open.target.clone();
            let commit = {
                let pump = pump.clone();
                let generation = Rc::clone(&generation);
                move |title: String| {
                    if !generation.borrow().is_current(token) || *busy.peek() {
                        return;
                    }
                    busy.set(true);
                    commit_rename(
                        pump.clone(),
                        target.clone(),
                        committed_name(&title),
                        busy,
                        Rc::clone(&generation),
                        token,
                    );
                }
            };
            let folder_target = matches!(open.target, RenameTarget::FolderWorkspace { .. });
            let title = open
                .headline
                .clone()
                .unwrap_or_else(|| RENAME_TERMINAL_HEADLINE.to_owned());
            let mut reset = commit.clone();
            let mut confirm = commit.clone();
            let mut on_enter = commit;
            rsx! {
                Dialog { title, size: "sm", close_button: Some(true),
                    TextField {
                        sx: field_sx.clone(),
                        label: "Custom name",
                        value: name(),
                        oninput: move |value| name.set(value),
                        attributes: vec![Attribute::new("autofocus", true, None, false)],
                        onkeydown: move |event: KeyboardEvent| {
                            if event.key() == Key::Named(NamedKey::Enter) {
                                event.prevent_default();
                                on_enter(name.peek().clone());
                            }
                        },
                    }
                    Text { size: "sm", color: "text-dimmed",
                        if folder_target {
                            "Names this folder as a workspace. Shows on every device you're signed in on."
                        } else {
                            "Overrides the auto title. Stays until you change it — agent/terminal title updates won't touch it. Clear it to go back to auto."
                        }
                    }
                    Flex { direction: "row", sx: footer_sx.clone(),
                        if open.has_custom {
                            Button {
                                "data-testid": "rename-reset",
                                variant: "outlined",
                                onclick: move |_| reset(String::new()),
                                "Reset to auto"
                            }
                        }
                        Button {
                            variant: "outlined",
                            onclick: move |_| scope.close(),
                            "Cancel"
                        }
                        Button {
                            "data-testid": "rename-confirm",
                            disabled: busy(),
                            onclick: move |_| confirm(name.peek().clone()),
                            if busy() { "Renaming…" } else { "Rename" }
                        }
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
                    completion_pump.dispatch(ClientEvent::Shell(ShellIntent::CloseRenameDialog));
                }
            });
        }
    }));
    rsx! {}
}

/// Run the commit off the render path; close only the opening that sent it.
fn commit_rename(
    pump: Pump,
    target: RenameTarget,
    title: String,
    busy: Signal<bool>,
    generation: Rc<RefCell<OverlayGeneration>>,
    token: u64,
) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut busy = busy;
        match rename_call(&pump, &target, &title).await {
            Ok(()) if generation.borrow().is_current(token) => {
                pump.dispatch(ClientEvent::Shell(ShellIntent::CloseRenameDialog));
            }
            Ok(()) => {}
            Err(message) if generation.borrow().is_current(token) => {
                tracing::warn!(target: "shell", %message, "rename failed");
                pump.dispatch(ClientEvent::Shell(ShellIntent::ActionFailed { message }));
                busy.set(false);
            }
            Err(_) => {}
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, target, title, busy, generation, token);
}

#[cfg(target_arch = "wasm32")]
async fn rename_call(pump: &Pump, target: &RenameTarget, title: &str) -> Result<(), String> {
    use roost_client_core::client::rpc::calls::sessions::RenameSession;
    use roost_client_core::client::rpc::calls::workspaces::{CreateWorkspace, UpdateWorkspace};
    use roost_client_core::store::sidebar::folder_groups::workspace_for_folder;

    let rpc = pump.rpc();
    let failed =
        |error: roost_client_core::client::rpc::CallError| format!("Rename failed: {error}");
    match target {
        RenameTarget::Session { session_id } => {
            let request = RenameSession {
                session_id: session_id.clone(),
                title: title.to_owned(),
            };
            match rpc.call(&request).await.map_err(failed)? {
                true => Ok(()),
                false => Err("Rename failed: session not found".to_owned()),
            }
        }
        RenameTarget::FolderWorkspace {
            worker_fp,
            folder_path,
            session_ids,
        } => {
            // A workspace name is at least one character; empty is a no-op.
            if title.is_empty() {
                return Ok(());
            }
            let existing = {
                let core = pump.core();
                let core = core.borrow();
                workspace_for_folder(
                    core.store(),
                    &crate::platform::worker_paths::BrowserWorkerPaths,
                    worker_fp,
                    folder_path,
                )
                .map(|workspace| (workspace.id.as_str().to_owned(), workspace.version))
            };
            match existing {
                Some((id, version)) => {
                    let request = UpdateWorkspace {
                        id,
                        if_version: u64::try_from(version).unwrap_or_default(),
                        name: Some(title.to_owned()),
                        ..UpdateWorkspace::default()
                    };
                    rpc.call(&request).await.map(|_| ()).map_err(failed)
                }
                None => {
                    let request = CreateWorkspace {
                        worker_fp: worker_fp.clone(),
                        name: title.to_owned(),
                        folder_path: folder_path.clone(),
                        color: None,
                        attach_session_ids: session_ids.clone(),
                    };
                    rpc.call(&request).await.map(|_| ()).map_err(failed)
                }
            }
        }
    }
}
