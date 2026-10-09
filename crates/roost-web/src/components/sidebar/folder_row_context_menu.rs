//! The folder row's right-click menu: rename the workspace, launch a terminal
//! or built-in agent in the folder, or hand off to the machine's OS actions.
//! The rename commits in `RenameDialogHost`; `FolderRow` opens this menu.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::store::shell_dialogs::{RenameDialogRequest, RenameTarget};
use roost_client_core::store::shell_intent::ShellIntent;
use roost_client_core::store::sidebar::folder_groups::workspace_for_folder;

use super::context_menu_frame::ContextMenuFrame;
use super::machine_action_items::MachineActionItems;
use crate::components::agent_chat::launch_agent;
use crate::components::browse::listing;
use crate::components::context_menu::CtxMenuItem;
use crate::machine_actions::MachineMenuKind;
use crate::platform::BrowserWorkerPaths;
use crate::pump::use_store;
use crate::router_state::use_navigate;

/// What the folder menu acts on, snapshotted when it opened.
#[derive(Debug, Clone, PartialEq)]
pub struct FolderMenuTarget {
    /// Viewport x.
    pub x: f64,
    /// Viewport y.
    pub y: f64,
    /// The folder's machine.
    pub worker_fp: String,
    /// The folder.
    pub folder_path: String,
    /// The row's label, the rename pre-fill when no workspace exists yet.
    pub display_name: String,
    /// Attached to the workspace on its first create.
    pub session_ids: Vec<String>,
}

/// The folder menu.
#[component]
pub fn FolderRowContextMenu(target: FolderMenuTarget, on_close: EventHandler<()>) -> Element {
    let pump = use_store();
    let navigate = use_navigate();
    let agent_enabled = pump
        .core()
        .borrow()
        .store()
        .coord_identity
        .as_ref()
        .is_some_and(|identity| identity.builtin_agent_enabled);
    let agent_worker_fp = target.worker_fp.clone();
    let agent_folder = target.folder_path.clone();
    let agent_pump = pump.clone();
    let agent_navigate = navigate;
    let on_new_agent = move |_: MouseEvent| {
        on_close.call(());
        launch_agent(
            agent_pump.clone(),
            agent_worker_fp.clone(),
            agent_folder.clone(),
            agent_navigate,
        );
    };
    let terminal_worker_fp = target.worker_fp.clone();
    let terminal_folder = target.folder_path.clone();
    let terminal_pump = pump.clone();
    let terminal_navigate = navigate;
    let on_new_terminal = move |_: MouseEvent| {
        on_close.call(());
        listing::launch_terminal(
            terminal_pump.clone(),
            terminal_worker_fp.clone(),
            terminal_folder.clone(),
            terminal_navigate,
        );
    };
    let machine_fp = target.worker_fp.clone();
    let rename = move |_| {
        let current_title = {
            let core = pump.core();
            let core = core.borrow();
            workspace_for_folder(
                core.store(),
                &BrowserWorkerPaths,
                &target.worker_fp,
                &target.folder_path,
            )
            .map(|workspace| workspace.name.clone())
            .unwrap_or_else(|| target.display_name.clone())
        };
        on_close.call(());
        pump.dispatch(ClientEvent::Shell(ShellIntent::OpenRenameDialog(
            RenameDialogRequest {
                current_title,
                has_custom: false,
                headline: Some("Rename workspace".to_owned()),
                target: RenameTarget::FolderWorkspace {
                    worker_fp: target.worker_fp.clone(),
                    folder_path: target.folder_path.clone(),
                    session_ids: target.session_ids.clone(),
                },
            },
        )));
    };
    rsx! {
        ContextMenuFrame {
            x: target.x,
            y: target.y,
            menu_id: "folder-context-menu",
            label: "Workspace actions",
            test_id: "folder-context-menu",
            on_close,
            CtxMenuItem { testid: "folder-ctx-rename", onclick: rename, "Rename…" }
            if agent_enabled {
                CtxMenuItem { testid: "folder-ctx-new-agent", onclick: on_new_agent, "New agent here" }
            }
            CtxMenuItem { testid: "folder-ctx-new-terminal", onclick: on_new_terminal, "New terminal here" }
            MachineActionItems {
                worker_fp: machine_fp,
                menu: MachineMenuKind::Folder,
                test_id_prefix: "folder-ctx",
                on_close,
            }
        }
    }
}
