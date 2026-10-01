//! What pressing a catalog row does, decided before anything is performed.
//! Ports the action half of `CommandPaletteBody.tsx`'s `selectItem`; the list
//! itself stays in `palette::body`.
//!
//! The decision is split from the doing because the rule a reader gets hurt by
//! is not "which shell intent opens the editor" — it is that a row carrying BOTH
//! a route and a command must do exactly one of them. A navigation that also
//! opened an editor leaves the editor mounted over the route it just went to,
//! and the two surfaces then answer to the same Escape.
//!
//! Each outcome names one surface, and the host is the only thing that can
//! perform one (`roost_client_core::store::palette::PaletteAction`).

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::store::palette::{PaletteAction, PaletteItem, captured_generation};
use roost_client_core::store::root::captured_generation_is_current;
use roost_client_core::store::shell_intent::ShellIntent;

use crate::pump::Pump;

/// What an action row said when its credential was retired underneath it.
const STALE_CREDENTIAL_MESSAGE: &str =
    "That command belongs to a session you are no longer signed in to.";

/// The surface one press goes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteOutcome {
    /// The route the reader is taken to.
    Navigate(String),
    /// The task editor, opened on a folder.
    QueueFolderTask {
        /// The machine the task runs on.
        worker_fp: String,
        /// The folder it runs in.
        cwd: String,
    },
    /// A terminal spawned beside this one.
    SpawnSibling {
        /// The machine to spawn on.
        worker_fp: String,
        /// The folder to spawn in.
        cwd: String,
    },
    /// Nothing: the row names neither a route nor a command.
    Nothing,
}

impl PaletteOutcome {
    /// Whether this press runs a command rather than taking a route.
    ///
    /// Only a command carries a captured credential, so only a command can
    /// outlive one.
    pub fn is_command(&self) -> bool {
        !matches!(self, Self::Navigate(_) | Self::Nothing)
    }
}

/// The surface `item` goes to: a route wins over a command.
///
/// A row that somehow carries both navigates and does NOT also run its command;
/// the editor or the spawn is the half that would strand the reader over the
/// route they just asked for.
pub fn palette_outcome(item: &PaletteItem) -> PaletteOutcome {
    if let Some(href) = item.href.clone() {
        return PaletteOutcome::Navigate(href);
    }
    match item.action.clone() {
        Some(PaletteAction::QueueFolderTask { worker_fp, cwd }) => {
            PaletteOutcome::QueueFolderTask { worker_fp, cwd }
        }
        Some(PaletteAction::SpawnSibling { worker_fp, cwd }) => {
            PaletteOutcome::SpawnSibling { worker_fp, cwd }
        }
        None => PaletteOutcome::Nothing,
    }
}

/// Perform the press, refusing a command whose credential has been retired.
pub fn perform(pump: &Pump, navigate: &EventHandler<String>, item: &PaletteItem) {
    let outcome = palette_outcome(item);
    if outcome.is_command() && !generation_is_current(pump, item) {
        let message = STALE_CREDENTIAL_MESSAGE;
        tracing::warn!(target: "palette", %message, "a captured action row outlived its credential");
        pump.dispatch(ClientEvent::Shell(ShellIntent::ActionFailed {
            message: STALE_CREDENTIAL_MESSAGE.to_owned(),
        }));
        return;
    }
    match outcome {
        PaletteOutcome::Navigate(href) => navigate.call(href),
        PaletteOutcome::QueueFolderTask { worker_fp, cwd } => {
            pump.dispatch(ClientEvent::Shell(ShellIntent::OpenQueueTaskDialog {
                cwd: Some(cwd),
                body: None,
                worker_fp: Some(worker_fp),
            }));
        }
        PaletteOutcome::SpawnSibling { worker_fp, cwd } => {
            spawn_sibling(pump, navigate, worker_fp, cwd);
        }
        PaletteOutcome::Nothing => {}
    }
}

/// Whether the credential this row captured is still the browser's credential.
fn generation_is_current(pump: &Pump, item: &PaletteItem) -> bool {
    let Some(captured) = captured_generation(item) else {
        return true;
    };
    let core = pump.core();
    let core = core.borrow();
    captured_generation_is_current(core.store(), captured)
}

/// Spawn the sibling terminal off the render path, then follow it.
fn spawn_sibling(pump: &Pump, navigate: &EventHandler<String>, worker_fp: String, cwd: String) {
    #[cfg(target_arch = "wasm32")]
    {
        let pump = pump.clone();
        let navigate = *navigate;
        wasm_bindgen_futures::spawn_local(async move {
            match spawn_sibling_call(&pump, &worker_fp, &cwd).await {
                Ok(session_id) => navigate.call(crate::routes::session_href(&session_id)),
                Err(error) => {
                    let message = format!("New terminal failed: {error}");
                    tracing::warn!(target: "palette", %message, "the sibling spawn failed");
                    pump.dispatch(ClientEvent::Shell(ShellIntent::ActionFailed { message }));
                }
            }
        });
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, navigate, worker_fp, cwd);
}

#[cfg(target_arch = "wasm32")]
async fn spawn_sibling_call(pump: &Pump, worker_fp: &str, cwd: &str) -> Result<String, String> {
    use roost_client_core::client::rpc::calls::sessions::SpawnSession;

    let request = SpawnSession {
        worker_fp: worker_fp.to_owned(),
        kind: "shell".to_owned(),
        folder: cwd.to_owned(),
        cols: None,
        rows: None,
        session_id: None,
    };
    pump.rpc()
        .call(&request)
        .await
        .map(|spawned| spawned.session_id)
        .map_err(|error| error.to_string())
}
