//! The folder row's right-click menu: "Rename…" names the folder's workspace
//! (creating it, with the folder's sessions attached, on the first rename),
//! then the machine's OS hand-offs (Screen sharing / Remote Desktop). Ports
//! `apps/web/src/components/sidebar/FolderRowContextMenu.tsx`; `FolderRow`
//! opens it. The rename commits in `RenameDialogHost`.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::store::shell_dialogs::{RenameDialogRequest, RenameTarget};
use roost_client_core::store::shell_intent::ShellIntent;
use roost_client_core::store::sidebar::folder_groups::workspace_for_folder;

use super::context_menu_frame::ContextMenuFrame;
use super::machine_action_items::MachineActionItems;
use crate::components::context_menu::CtxMenuItem;
use crate::machine_actions::MachineMenuKind;
use crate::platform::BrowserWorkerPaths;
use crate::pump::use_store;

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
            MachineActionItems {
                worker_fp: machine_fp,
                menu: MachineMenuKind::Folder,
                test_id_prefix: "folder-ctx",
                on_close,
            }
        }
    }
}
