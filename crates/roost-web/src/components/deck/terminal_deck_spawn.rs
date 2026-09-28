//! The deck's spawn flows: a new tab in a pane and a split beside the focused
//! pane. Each asks `SessionsSpawn` for a shell in the anchor terminal's folder,
//! waits briefly for the coordinator's row, and lands it through
//! `DeckSpawn::landed`; a refusal raises `DeckSpawn::refused`'s card. Called by
//! `terminal_deck_operations`. Ports the async half of `newTab` and `split` in
//! `apps/web/src/components/deck/terminal-deck-operations.ts`.

use roost_client_core::Store;
use roost_client_core::deck::DeckSpawn;
use roost_client_core::store::layout::PaneView;
use roost_client_core::store::selectors::session_by_id;
use roost_client_core::store::sidebar::folder_groups::folder_path_of;

use crate::pump::Pump;

/// The shell a deck spawn asks for: the anchor terminal's machine and folder,
/// and the session whose folder bucket the new terminal lands in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnAnchor {
    /// The terminal the spawn copies.
    pub session_id: String,
    /// Its machine.
    pub worker_fp: String,
    /// Its folder (`folderPathOf`: the live cwd).
    pub folder: String,
}

/// The terminal a spawn from `pane_id` copies: the pane's selected tab, else
/// the session the deck follows.
pub fn spawn_anchor(
    store: &Store,
    panes: &[PaneView],
    pane_id: &str,
    followed: Option<&str>,
) -> Option<SpawnAnchor> {
    let selected = panes
        .iter()
        .find(|pane| pane.pane_id == pane_id)
        .and_then(|pane| session_by_id(store, &pane.selected_tab));
    let session = selected.or_else(|| followed.and_then(|id| session_by_id(store, id)))?;
    Some(SpawnAnchor {
        session_id: session.id.as_str().to_owned(),
        worker_fp: session.worker_fp.as_str().to_owned(),
        folder: folder_path_of(session).to_owned(),
    })
}

/// Run one spawn off the render path.
pub fn start_deck_spawn(pump: Pump, spawn: DeckSpawn, anchor: SpawnAnchor, compact: bool) {
    tracing::info!(
        target: "deck",
        kind = spawn.kind_name(),
        pane_id = spawn.pane_id(),
        worker_fp = %anchor.worker_fp,
        "deck spawn requested"
    );
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(web::run_deck_spawn(pump, spawn, anchor, compact));
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, spawn, anchor, compact);
}

#[cfg(target_arch = "wasm32")]
mod web {
    use roost_client_core::ClientEvent;
    use roost_client_core::client::rpc::calls::sessions::SpawnSession;
    use roost_client_core::deck::DeckSpawn;
    use roost_client_core::store::selectors::session_by_id;

    use super::SpawnAnchor;
    use crate::components::deck::terminal_deck_model::deck_folder_for;
    use crate::components::terminal::dom::sleep_ms;
    use crate::platform::worker_paths::BrowserWorkerPaths;
    use crate::pump::Pump;

    /// v2 `waitForSession`: poll every 25ms for up to 2s, then land anyway —
    /// the layout reconciles the tab once its row arrives.
    const ROW_WAIT_STEP_MS: u64 = 25;
    const ROW_WAIT_STEPS: u32 = 80;

    pub(super) async fn run_deck_spawn(
        pump: Pump,
        spawn: DeckSpawn,
        anchor: SpawnAnchor,
        compact: bool,
    ) {
        let request = SpawnSession {
            worker_fp: anchor.worker_fp.clone(),
            kind: "shell".to_owned(),
            folder: anchor.folder.clone(),
            cols: None,
            rows: None,
            session_id: None,
        };
        let spawned = match pump.rpc().call(&request).await {
            Ok(spawned) => spawned,
            Err(error) => {
                let error = error.to_string();
                tracing::warn!(target: "deck", kind = spawn.kind_name(), %error, "deck spawn refused");
                pump.dispatch(spawn.refused(&error));
                return;
            }
        };
        let session_id = spawned.session_id;
        for _ in 0..ROW_WAIT_STEPS {
            if session_by_id(pump.core().borrow().store(), &session_id).is_some() {
                break;
            }
            sleep_ms(ROW_WAIT_STEP_MS).await;
        }
        let folder = {
            let core = pump.core();
            let core = core.borrow();
            let store = core.store();
            session_by_id(store, &session_id)
                .or_else(|| session_by_id(store, &anchor.session_id))
                .map(|landed| deck_folder_for(store, &BrowserWorkerPaths, landed))
        };
        let Some(folder) = folder else {
            tracing::warn!(target: "deck", session_id, "deck spawn landed with neither its row nor its anchor live");
            return;
        };
        tracing::info!(target: "deck", kind = spawn.kind_name(), session_id, "deck spawn landed");
        pump.dispatch(ClientEvent::Deck(spawn.landed(folder, session_id, compact)));
    }
}
