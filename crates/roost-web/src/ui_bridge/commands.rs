//! Draining the Sync-queued UI commands and performing each one as the browser
//! operations it stands for.
//!
//! Ports `handleUiCommand` from `apps/web/src/lib/uiCommandDispatch.ts`. The
//! resolution — which tab a frame is for, whether the session it names is open,
//! and what each command means for a folder — is `client::ui_command::dispatch`;
//! this file is the half that needs a router and a deck.
//!
//! EVERY LEGACY COMMAND IS A DECK INTENT, not a private code path. `arrange`
//! from a CLI, the arrange menu and a keyboard chord therefore commit the same
//! arrangement, and a bug fixed for one is fixed for all three.

use roost_client_core::client::ui_command::drain_ui_commands as drain_queued_commands;
use roost_client_core::client::ui_command::{
    LayoutReshape, UiCommandAction, UiCommandScope, folder_live_session_ids, reshape_folder_layout,
};
use roost_client_core::deck::{DeckFolder, DeckIntent};
use roost_client_core::store::layout::{LAYOUT_STORAGE_KEY, find_leaf_of_tab};
use roost_client_core::store::selectors::session_by_id;
use roost_client_core::{ClientEvent, Store};

use crate::platform::worker_paths::BrowserWorkerPaths;
use crate::pump::Pump;
use crate::routes::session_href;
use crate::session_actions::close_labels_for;
use crate::ui_bridge::apply::run_acknowledged_layout_apply;
use crate::ui_bridge::host::{ShellFacts, UiBridgeHost};

/// Pop every queued UI command at `path` and perform what each one means.
pub fn drain_ui_commands(pump: &Pump, host: &mut dyn UiBridgeHost, shell: &ShellFacts) {
    let core = pump.core();
    // The scope is read before the mutable borrow, because `own_tab_id` and
    // `active_session_id` are values the drain compares against and neither
    // may borrow the store the drain is about to empty.
    let (own_tab_id, active_session_id) = {
        let core = core.borrow();
        let store = core.store();
        (
            store.tab_id.clone(),
            crate::route_session::active_session_for_path(store, &BrowserWorkerPaths, &shell.path)
                .map(|session| session.id.as_str().to_owned()),
        )
    };
    let actions = {
        let mut core = core.borrow_mut();
        let scope = UiCommandScope {
            own_tab_id: own_tab_id.as_str(),
            paths: &BrowserWorkerPaths,
            active_session_id: active_session_id.as_deref(),
        };
        drain_queued_commands(core.store_mut(), &scope)
    };
    if actions.is_empty() {
        return;
    }
    for action in actions {
        perform(pump, host, shell, active_session_id.as_deref(), action);
    }
}

/// Perform one drained action.
fn perform(
    pump: &Pump,
    host: &mut dyn UiBridgeHost,
    shell: &ShellFacts,
    active_session_id: Option<&str>,
    action: UiCommandAction,
) {
    match action {
        UiCommandAction::Navigate { path } => host.navigate(&path),
        UiCommandAction::SelectTab {
            folder_key,
            session_id,
        } => {
            let Some(folder) = deck_folder(pump, &folder_key) else {
                return;
            };
            pump.dispatch(ClientEvent::Deck(DeckIntent::SelectTab {
                folder,
                session_id,
                compact: shell.compact,
            }));
        }
        UiCommandAction::FocusPane {
            folder_key,
            session_id,
        } => focus_pane(pump, shell, &folder_key, &session_id),
        UiCommandAction::CloseTab {
            folder_key,
            session_id,
        } => close_tab(pump, active_session_id, &folder_key, &session_id),
        UiCommandAction::ReshapeLayout {
            folder_key,
            live_session_ids,
            command,
        } => reshape_layout(pump, host, &folder_key, &live_session_ids, &command),
        UiCommandAction::ApplyLayout(command) => {
            run_acknowledged_layout_apply(pump, host, &shell.path, &command);
        }
    }
}

/// The folder bucket an action names, with the membership a deck intent
/// reconciles against — this browser's placeholders included, so a client-only
/// spawn is in the arrangement rather than pruned out of the next resolve.
fn deck_folder(pump: &Pump, folder_key: &str) -> Option<DeckFolder> {
    if folder_key.is_empty() {
        return None;
    }
    let core = pump.core();
    let core = core.borrow();
    Some(DeckFolder {
        folder_key: folder_key.to_owned(),
        live_session_ids: folder_live_session_ids(core.store(), &BrowserWorkerPaths, folder_key),
    })
}

/// Focus the pane HOLDING `session_id`, which is not the pane showing it.
///
/// v2's `focusPaneOp` navigates to the addressed pane's own selected tab, so
/// the resolved pane id is what the intent takes and the route follows the
/// intent's own navigation.
fn focus_pane(pump: &Pump, shell: &ShellFacts, folder_key: &str, session_id: &str) {
    let Some(folder) = deck_folder(pump, folder_key) else {
        return;
    };
    let pane_id = {
        let core = pump.core();
        let core = core.borrow();
        let layout = core.store().deck.resolve_layout(&folder);
        find_leaf_of_tab(&layout.root, session_id).map(|leaf| leaf.pane_id.clone())
    };
    let Some(pane_id) = pane_id else {
        tracing::warn!(target: "ui_cc", kind = "focus_pane", session_id, "ui_command_unknown_session");
        return;
    };
    pump.dispatch(ClientEvent::Deck(DeckIntent::FocusPane {
        folder,
        pane_id,
        compact: shell.compact,
    }));
}

/// The tab ✕'s own close, with its undo window and the view it lands on.
fn close_tab(pump: &Pump, active_session_id: Option<&str>, folder_key: &str, session_id: &str) {
    let folder = deck_folder(pump, folder_key);
    let labels = {
        let core = pump.core();
        let core = core.borrow();
        match session_by_id(core.store(), session_id) {
            Some(session) => close_labels_for(core.store(), session),
            None => {
                tracing::warn!(target: "ui_cc", kind = "close_tab", session_id, "ui_command_unknown_session");
                return;
            }
        }
    };
    pump.dispatch(ClientEvent::Deck(DeckIntent::CloseTab {
        folder,
        session_id: session_id.to_owned(),
        active_session_id: active_session_id.map(str::to_owned),
        labels,
    }));
}

/// Resolve the folder's arrangement, map `command` onto it, commit, persist, and
/// follow the route the reshape asked for.
fn reshape_layout(
    pump: &Pump,
    host: &mut dyn UiBridgeHost,
    folder_key: &str,
    live_session_ids: &[String],
    command: &roost_client_core::client::ui_command::LegacyUiCommand,
) {
    let payload = pump.write_store(|store: &mut Store| {
        let (records, ids) = store.deck.layout_state();
        let reshape = reshape_folder_layout(records, ids, folder_key, live_session_ids, command);
        let LayoutReshape::Committed {
            navigate_to_session,
        } = reshape
        else {
            return None;
        };
        if let Some(session_id) = navigate_to_session {
            host.navigate(&session_href(&session_id));
        }
        match store.deck.records().snapshot() {
            Ok(payload) => Some(payload),
            Err(error) => {
                tracing::warn!(target: "layout", %error, "reshaped pane layout not persisted");
                None
            }
        }
    });
    // The write is a SECOND borrow on purpose: the commit needed the store
    // mutably and the key/value store hangs off the same core, so the payload
    // is carried out rather than read from a store this borrow still holds.
    if let Some(payload) = payload {
        let core = pump.core();
        let core = core.borrow();
        core.storage().set(LAYOUT_STORAGE_KEY, &payload);
    }
}
