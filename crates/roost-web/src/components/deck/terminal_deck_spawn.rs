//! The deck's terminal spawn flows: a new tab or split beside the focused pane.
//! The spawn copies the anchor session's folder and lands after its row arrives.
//! Called by `terminal_deck_operations` and the terminal context menu.

use roost_client_core::Store;
use roost_client_core::deck::DeckSpawn;
use roost_client_core::store::layout::PaneView;
use roost_client_core::store::selectors::session_by_id;
use roost_client_core::store::sidebar::folder_groups::folder_path_of;

use crate::pump::Pump;

/// Where a deck spawn lands: the anchor tab's machine and folder, and the tab
/// whose folder bucket the new tab joins when its own row is late.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnAnchor {
    /// The session the spawn copies.
    pub tab_id: String,
    /// Its machine.
    pub worker_fp: String,
    /// Its live folder.
    pub folder: String,
}

/// The tab a spawn from `pane_id` copies: the pane's selected tab, else the
/// tab the deck follows.
pub fn spawn_anchor(
    store: &Store,
    panes: &[PaneView],
    pane_id: &str,
    followed: Option<&str>,
) -> Option<SpawnAnchor> {
    let selected = panes
        .iter()
        .find(|pane| pane.pane_id == pane_id)
        .and_then(|pane| anchor_for_tab(store, &pane.selected_tab));
    selected.or_else(|| followed.and_then(|tab_id| anchor_for_tab(store, tab_id)))
}

fn anchor_for_tab(store: &Store, tab_id: &str) -> Option<SpawnAnchor> {
    session_by_id(store, tab_id).map(|session| SpawnAnchor {
        tab_id: tab_id.to_owned(),
        worker_fp: session.worker_fp.as_str().to_owned(),
        folder: folder_path_of(session).to_owned(),
    })
}

/// The deck folder an anchor tab belongs to.
#[cfg(target_arch = "wasm32")]
fn anchor_folder(
    store: &Store,
    anchor: &SpawnAnchor,
) -> Option<roost_client_core::deck::DeckFolder> {
    use crate::components::deck::terminal_deck_model::deck_folder_for;
    use crate::platform::worker_paths::BrowserWorkerPaths;

    session_by_id(store, &anchor.tab_id)
        .map(|session| deck_folder_for(store, &BrowserWorkerPaths, session))
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
                .map(|landed| deck_folder_for(store, &BrowserWorkerPaths, landed))
                .or_else(|| super::anchor_folder(store, &anchor))
        };
        let Some(folder) = folder else {
            tracing::warn!(target: "deck", session_id, "deck spawn landed with neither its row nor its anchor live");
            return;
        };
        tracing::info!(target: "deck", kind = spawn.kind_name(), session_id, "deck spawn landed");
        pump.dispatch(ClientEvent::Deck(spawn.landed(folder, session_id, compact)));
    }
}
