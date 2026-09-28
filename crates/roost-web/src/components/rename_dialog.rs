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

use crate::components::md::{Button, ButtonVariant, Dialog, TextField};
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
    let Some(open) = request else {
        return rsx! {};
    };
    let commit = {
        let pump = pump.clone();
        let target = open.target.clone();
        move |title: String| {
            if *busy.peek() {
                return;
            }
            busy.set(true);
            commit_rename(pump.clone(), target.clone(), committed_name(&title), busy);
        }
    };
    let close = {
        let pump = pump.clone();
        move |()| pump.dispatch(ClientEvent::Shell(ShellIntent::CloseRenameDialog))
    };
    let cancel = close.clone();
    let folder_target = matches!(open.target, RenameTarget::FolderWorkspace { .. });
    let mut reset = commit.clone();
    let mut confirm = commit.clone();
    let mut on_enter = commit;
    rsx! {
        Dialog {
            open: true,
            on_close: close,
            headline: open.headline.clone().unwrap_or_else(|| RENAME_TERMINAL_HEADLINE.to_owned()),
            actions: rsx! {
                if open.has_custom {
                    Button {
                        variant: ButtonVariant::Ghost,
                        "data-testid": "rename-reset",
                        onclick: move |_| reset(String::new()),
                        "Reset to auto"
                    }
                }
                span { style: "flex: 1;" }
                Button { variant: ButtonVariant::Outline, onclick: move |_| cancel(()), "Cancel" }
                Button {
                    variant: ButtonVariant::Default,
                    "data-testid": "rename-confirm",
                    disabled: busy(),
                    onclick: move |_| confirm(name.peek().clone()),
                    if busy() { "Renaming…" } else { "Rename" }
                }
            },
            div { style: "display: flex; flex-direction: column; gap: var(--md-space-3); min-width: min(320px, 80vw);",
                TextField {
                    value: name(),
                    on_input: move |value: String| name.set(value),
                    label: "Custom name",
                    test_id: "rename-input",
                    style: "width: 100%;",
                    autofocus: true,
                    onkeydown: move |event: KeyboardEvent| {
                        if event.key() == Key::Enter {
                            event.prevent_default();
                            on_enter(name.peek().clone());
                        }
                    },
                }
                div { class: "md-body-s", style: "color: var(--md-sys-color-on-surface-variant);",
                    if folder_target {
                        "Names this folder as a workspace. Shows on every device you're signed in on."
                    } else {
                        "Overrides the auto title. Stays until you change it — agent/terminal title updates won't touch it. Clear it to go back to auto."
                    }
                }
            }
        }
    }
}

/// Run the commit off the render path; close on success, toast on failure.
fn commit_rename(pump: Pump, target: RenameTarget, title: String, busy: Signal<bool>) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut busy = busy;
        match rename_call(&pump, &target, &title).await {
            Ok(()) => pump.dispatch(ClientEvent::Shell(ShellIntent::CloseRenameDialog)),
            Err(message) => {
                tracing::warn!(target: "shell", %message, "rename failed");
                pump.dispatch(ClientEvent::Shell(ShellIntent::ActionFailed { message }));
                busy.set(false);
            }
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, target, title, busy);
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
