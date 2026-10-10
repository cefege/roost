//! The deck's spawn flows: a new terminal or agent tab in a pane and a split
//! beside the focused pane. A terminal spawn asks `SessionsSpawn` for a shell
//! in the anchor tab's folder, waits briefly for the coordinator's row, lands
//! it through `DeckSpawn::landed`, and — when the launcher configuration says
//! so — types the configured agent's command into the fresh PTY once; an agent
//! spawn does the same with `AgentChatCreate`. The anchor may be a terminal or
//! an agent tab. A refusal raises `DeckSpawn::refused`'s card. Called by
//! `terminal_deck_operations` and the terminal context menu.

use roost_client_core::Store;
use roost_client_core::deck::DeckSpawn;
use roost_client_core::deck::tab::DeckTab;
use roost_client_core::store::layout::PaneView;
use roost_client_core::store::selectors::session_by_id;
use roost_client_core::store::sidebar::folder_groups::folder_path_of;

use crate::pump::Pump;

/// Where a deck spawn lands: the anchor tab's machine and folder, and the tab
/// whose folder bucket the new tab joins when its own row is late.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnAnchor {
    /// The deck tab the spawn copies: a session id or `agent:<id>`.
    pub tab_id: String,
    /// Its machine.
    pub worker_fp: String,
    /// Its folder: a terminal's live cwd, an agent's starting folder.
    pub folder: String,
}

/// The tab a spawn from `pane_id` copies: the pane's selected tab, else the
/// tab the deck follows. Either may be a terminal or an agent conversation.
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
    match DeckTab::parse(tab_id)? {
        DeckTab::Terminal(_) => session_by_id(store, tab_id).map(|session| SpawnAnchor {
            tab_id: tab_id.to_owned(),
            worker_fp: session.worker_fp.as_str().to_owned(),
            folder: folder_path_of(session).to_owned(),
        }),
        DeckTab::Agent(conversation_id) => store
            .agent_chat
            .conversations
            .get(&conversation_id)
            .map(|conversation| SpawnAnchor {
                tab_id: tab_id.to_owned(),
                worker_fp: conversation.worker_fp.clone(),
                folder: conversation.cwd.clone(),
            }),
    }
}

/// The deck folder an anchor tab belongs to.
#[cfg(target_arch = "wasm32")]
fn anchor_folder(
    store: &Store,
    anchor: &SpawnAnchor,
) -> Option<roost_client_core::deck::DeckFolder> {
    use crate::components::deck::terminal_deck_model::{deck_folder_for, deck_folder_for_agent};
    use crate::platform::worker_paths::BrowserWorkerPaths;

    match DeckTab::parse(&anchor.tab_id)? {
        DeckTab::Terminal(_) => session_by_id(store, &anchor.tab_id)
            .map(|session| deck_folder_for(store, &BrowserWorkerPaths, session)),
        DeckTab::Agent(conversation_id) => store
            .agent_chat
            .conversations
            .get(&conversation_id)
            .map(|conversation| deck_folder_for_agent(store, &BrowserWorkerPaths, conversation)),
    }
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

/// Create an agent conversation and land its tab after Sync publishes its row.
pub fn start_deck_agent(pump: Pump, spawn: DeckSpawn, anchor: SpawnAnchor, compact: bool) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(web::run_deck_agent(pump, spawn, anchor, compact));
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, spawn, anchor, compact);
}

#[cfg(target_arch = "wasm32")]
mod web {
    use roost_client_core::ClientEvent;
    use roost_client_core::client::rpc::calls::agent_chat::CreateAgentChat;
    use roost_client_core::client::rpc::calls::sessions::SpawnSession;
    use roost_client_core::deck::{DeckSpawn, agent_tab_id};
    use roost_client_core::store::selectors::session_by_id;

    use super::SpawnAnchor;
    use crate::components::deck::terminal_deck_model::{deck_folder_for, deck_folder_for_agent};
    use crate::components::terminal::dom::sleep_ms;
    use crate::platform::worker_paths::BrowserWorkerPaths;
    use crate::pump::Pump;

    /// v2 `waitForSession`: poll every 25ms for up to 2s, then land anyway —
    /// the layout reconciles the tab once its row arrives.
    const ROW_WAIT_STEP_MS: u64 = 25;
    const ROW_WAIT_STEPS: u32 = 80;
    /// An agent row rides the host's event stream through the coordinator, and
    /// an agent tab landed before its row is pruned by the deck's reconcile,
    /// so it waits longer than a terminal's.
    const AGENT_ROW_WAIT_STEPS: u32 = 400;

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
        launch_configured_agent(&pump, &session_id);
        pump.dispatch(ClientEvent::Deck(spawn.landed(folder, session_id, compact)));
    }

    pub(super) async fn run_deck_agent(
        pump: Pump,
        spawn: DeckSpawn,
        anchor: SpawnAnchor,
        compact: bool,
    ) {
        let conversation = match pump
            .rpc()
            .call(&CreateAgentChat {
                worker_fp: anchor.worker_fp.clone(),
                cwd: anchor.folder.clone(),
                model: None,
            })
            .await
        {
            Ok(conversation) => conversation,
            Err(error) => {
                let message = error.to_string();
                tracing::warn!(target: "deck", kind = spawn.kind_name(), %message, "agent tab creation refused");
                pump.dispatch(spawn.refused(&message));
                return;
            }
        };
        for _ in 0..AGENT_ROW_WAIT_STEPS {
            if pump
                .core()
                .borrow()
                .store()
                .agent_chat
                .conversations
                .contains_key(&conversation.id)
            {
                break;
            }
            sleep_ms(ROW_WAIT_STEP_MS).await;
        }
        let folder = {
            let core = pump.core();
            let core = core.borrow();
            core.store()
                .agent_chat
                .conversations
                .get(&conversation.id)
                .map(|conversation| {
                    deck_folder_for_agent(core.store(), &BrowserWorkerPaths, conversation)
                })
                .or_else(|| super::anchor_folder(core.store(), &anchor))
        };
        let Some(folder) = folder else {
            tracing::warn!(target: "deck", conversation_id = %conversation.id, "agent tab landed without its folder row");
            return;
        };
        pump.dispatch(ClientEvent::Deck(spawn.landed(
            folder,
            agent_tab_id(&conversation.id),
            compact,
        )));
    }

    pub(super) fn launch_configured_agent(pump: &Pump, session_id: &str) {
        let Some(command) = pump.core().borrow().store().agent_launcher.launch_command() else {
            return;
        };
        tracing::info!(target: "deck", session_id, %command, "auto-launching configured agent");
        let mut bytes = command.into_bytes();
        bytes.extend_from_slice(&roost_protocol::terminal_input::CR_BYTES);
        pump.dispatch(ClientEvent::TerminalInput {
            session_id: session_id.to_owned(),
            view_id: None,
            bytes,
        });
    }
}

/// Type the launcher's agent command into a freshly spawned PTY, once, when
/// the launcher configuration says a new terminal auto-launches. v2's
/// `maybeAutoLaunchAgent`: the command is `resolveAgent().command + "\r"`, and
/// a dropped batch is logged and NOT retried — a second attempt would
/// double-type an agent the first attempt may have started. Every spawn
/// surface routes through this one helper, so the deck's tabs, splits and
/// swipes and the browse launch all behave alike. A native build has no
/// coordinator-held configuration, so it never launches.
pub fn launch_configured_agent(pump: &Pump, session_id: &str) {
    #[cfg(target_arch = "wasm32")]
    web::launch_configured_agent(pump, session_id);
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, session_id);
}
