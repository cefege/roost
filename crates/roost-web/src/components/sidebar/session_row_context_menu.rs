//! A session row's right-click menu: Rename…, Duplicate terminal, Restart, the
//! machine's OS hand-offs (Finder / Screen sharing, network share / Remote
//! Desktop), and below a separator Close terminal (the row's own undoable
//! close) and, while the machine is offline, Force remove. Ports
//! `apps/web/src/components/sidebar/SessionRowContextMenu.tsx`; `SessionRow`
//! opens it. Spawn and kill go through `client::rpc::calls::sessions`.

use dioxus::prelude::*;
use libero::{
    components::{Button, Divider},
    sx::sx,
};
use roost_client_core::ClientEvent;
use roost_client_core::store::navigation::worker_online;
use roost_client_core::store::selectors::session_by_id;
use roost_client_core::store::shell_dialogs::{RenameDialogRequest, RenameTarget};
use roost_client_core::store::shell_intent::ShellIntent;

use super::context_menu_frame::ContextMenuFrame;
use super::machine_action_items::MachineActionItems;
use crate::machine_actions::MachineMenuKind;
use crate::pump::{Pump, use_store};
use crate::router_state::use_navigate;
use crate::session_naming::folder_headline;

/// What a session menu entry asks the coordinator for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionMenuCall {
    /// A second terminal in the same folder on the same machine.
    Duplicate,
    /// A fresh terminal in the same folder, then end this one.
    Restart,
    /// End this one without the worker's acknowledgement.
    ForceRemove,
}

impl SessionMenuCall {
    /// The toast prefix a failure reads with.
    pub const fn failure_prefix(self) -> &'static str {
        match self {
            Self::Duplicate => "Duplicate failed",
            Self::Restart => "Restart failed",
            Self::ForceRemove => "Force remove failed",
        }
    }
}

/// The session menu at (`x`, `y`).
#[component]
pub fn SessionRowContextMenu(
    session_id: String,
    x: f64,
    y: f64,
    on_close: EventHandler<()>,
    on_delete: EventHandler<()>,
) -> Element {
    let pump = use_store();
    let navigate = use_navigate();
    let item_sx = use_hook(|| {
        sx().width("100%")
            .justify_content("flex-start")
            .text_align("start")
    });
    let (rename_request, worker_offline, worker_fp) = {
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        let Some(session) = session_by_id(store, &session_id) else {
            return rsx! {};
        };
        let now_ms = i64::try_from(core.clock().now_ms()).unwrap_or(i64::MAX);
        let offline = store
            .workers
            .get(session.worker_fp.as_str())
            .is_none_or(|worker| {
                !worker_online(worker, store.routable_worker_fps.as_ref(), now_ms)
            });
        let request = RenameDialogRequest {
            current_title: session
                .custom_title
                .clone()
                .unwrap_or_else(|| folder_headline(store, session)),
            has_custom: session.custom_title.is_some(),
            headline: None,
            target: RenameTarget::Session {
                session_id: session_id.clone(),
            },
        };
        (request, offline, session.worker_fp.to_string())
    };
    let call = {
        let pump = pump.clone();
        let session_id = session_id.clone();
        move |kind: SessionMenuCall| {
            run_session_call(pump.clone(), session_id.clone(), kind, navigate);
        }
    };
    let rename_pump = pump.clone();
    let (duplicate, restart, force) = (call.clone(), call.clone(), call);
    rsx! {
        ContextMenuFrame {
            x,
            y,
            test_id: "session-context-menu-{session_id}",
            label: "Terminal actions",
            on_close,
            Button {
                sx: item_sx.clone(), variant: "standard", size: "sm", role: "menuitem", tabindex: "-1",
                "data-testid": "session-ctx-rename-{session_id}",
                onclick: move |_| {
                    rename_pump.dispatch(ClientEvent::Shell(ShellIntent::OpenRenameDialog(rename_request.clone())));
                    on_close.call(());
                },
                "Rename…"
            }
            Button {
                sx: item_sx.clone(), variant: "standard", size: "sm", role: "menuitem", tabindex: "-1",
                "data-testid": "session-ctx-duplicate-{session_id}",
                onclick: move |_| { duplicate(SessionMenuCall::Duplicate); on_close.call(()); },
                "Duplicate terminal"
            }
            Button {
                sx: item_sx.clone(), variant: "standard", size: "sm", role: "menuitem", tabindex: "-1",
                "data-testid": "session-ctx-restart-{session_id}",
                onclick: move |_| { restart(SessionMenuCall::Restart); on_close.call(()); },
                "Restart"
            }
            MachineActionItems {
                pump: pump.clone(),
                worker_fp,
                menu: MachineMenuKind::Session,
                test_id_prefix: "session-ctx",
                test_id_suffix: "-{session_id}",
                on_close,
            }
            Divider {}
            Button {
                sx: item_sx.clone(), variant: "standard", size: "sm", role: "menuitem", tabindex: "-1",
                "data-testid": "session-ctx-delete-{session_id}",
                onclick: move |_| { on_delete.call(()); on_close.call(()); },
                "Close terminal"
            }
            if worker_offline {
                Button {
                    sx: item_sx, variant: "standard", size: "sm", role: "menuitem", tabindex: "-1",
                    "data-testid": "session-ctx-force-remove-{session_id}",
                    onclick: move |_| { force(SessionMenuCall::ForceRemove); on_close.call(()); },
                    "Force remove (worker offline)"
                }
            }
        }
    }
}

/// Issue the call off the render path; navigate to a new session on success
/// and toast a failure.
fn run_session_call(
    pump: Pump,
    session_id: String,
    kind: SessionMenuCall,
    navigate: EventHandler<String>,
) {
    tracing::info!(target: "sidebar", session_id, call = kind.failure_prefix(), "session menu call");
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        match session_call(&pump, &session_id, kind).await {
            Ok(Some(next)) => navigate.call(format!("/s/{next}")),
            Ok(None) => {}
            Err(error) => {
                let message = format!("{}: {error}", kind.failure_prefix());
                tracing::warn!(target: "sidebar", %message, "session menu call failed");
                pump.dispatch(ClientEvent::Shell(ShellIntent::ActionFailed { message }));
            }
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, session_id, navigate);
}

#[cfg(target_arch = "wasm32")]
async fn session_call(
    pump: &Pump,
    session_id: &str,
    kind: SessionMenuCall,
) -> Result<Option<String>, String> {
    use roost_client_core::client::rpc::calls::sessions::{KillSession, SpawnSession};

    let rpc = pump.rpc();
    let kill = |force: bool| KillSession {
        session_id: session_id.to_owned(),
        force,
    };
    if kind == SessionMenuCall::ForceRemove {
        let accepted = rpc
            .call(&kill(true))
            .await
            .map_err(|error| error.to_string())?;
        return if accepted {
            Ok(None)
        } else {
            Err("not accepted".to_owned())
        };
    }
    let spawn = {
        let core = pump.core();
        let core = core.borrow();
        let session = session_by_id(core.store(), session_id).ok_or("the session is gone")?;
        SpawnSession {
            worker_fp: session.worker_fp.to_string(),
            kind: session.kind.as_str().to_owned(),
            folder: session.cwd.clone(),
            cols: None,
            rows: None,
            session_id: None,
        }
    };
    let spawned = rpc.call(&spawn).await.map_err(|error| error.to_string())?;
    if kind == SessionMenuCall::Restart {
        rpc.call(&kill(false))
            .await
            .map_err(|error| error.to_string())?;
    }
    Ok(Some(spawned.session_id))
}
