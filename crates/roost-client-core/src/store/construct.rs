//! `Store::new`: the empty store every `ClientCore` starts from.
//! Split from `store.rs`, which keeps the struct and its small accessors;
//! every field's zero value is wired here, so a new field fails to compile
//! until it gets one.

use super::*;

impl Store {
    /// An empty store, presenting `tab_id` on every dial.
    pub fn new(sync: SyncState, tab_id: impl Into<String>) -> Self {
        Self {
            revision: 0,
            frames_revision: 0,
            sessions: SessionPlane::new(),
            sync,
            client_state: ClientStateSync::default(),
            tab_id: tab_id.into(),
            hydrated: false,
            account_id: None,
            terminal: BTreeMap::new(),
            routes: RouteRegistry::new(),
            workers: BTreeMap::new(),
            auth_generation: 0,
            browser_access_state: BrowserAccessState::Checking,
            direct: crate::client::carriers::CarrierLane::new(),
            input: InputRouter::new(),
            find_results: BTreeMap::new(),
            next_call_id: 1,
            next_attempt_id: 1,
            agent_status: crate::client::agents::AgentStatusProjection::new(),
            agent_chat: crate::client::agent_chat::AgentChatState::default(),
            agent_seen: crate::client::agents::AgentSeenLedger::new(),
            agent_seen_dirty: false,
            browse: crate::store::browse_state::BrowseState::new(),
            global_search: crate::client::global_search::GlobalSearchController::new(),
            toasts: ToastStack::new(),
            transfers: TransferStack::new(),
            spotlight: Spotlight::new(),
            ui: UiState::new(),
            terminal_nav_pad: crate::store::terminal_nav_pad::TerminalNavPad::new(),
            sidebar: crate::store::sidebar::SidebarState::default(),
            pending_sync_view_rotation: BTreeMap::new(),
            shell_dialogs: crate::store::shell_dialogs::ShellDialogs::default(),
            deck: crate::deck::DeckState::new(0),
            coord_identity: None,
            prefs: Prefs::new(),
            spawns: SpawnLedger::new(),
            pending_closes: PendingCloses::new(),
            mcp_relays: BTreeMap::new(),
            agent_launcher: crate::store::agent_launcher::AgentLauncherState::default(),
            pair_requests: BTreeMap::new(),
            workspaces: BTreeMap::new(),
            tasks: BTreeMap::new(),
            routable_worker_fps: None,
            routable_assembly: RoutableAssembly::new(),
            terminal_titles: BTreeMap::new(),
            last_activity_ms: BTreeMap::new(),
            session_viewers: BTreeMap::new(),
            presence_notices: VecDeque::new(),
            audit_rows: VecDeque::new(),
            ui_commands: VecDeque::new(),
            terminal_clipboard_requests: clipboard_requests::TerminalClipboardRequests::default(),
            clipboard_history: clipboard_history::ClipboardHistory::default(),
            command_finished_requests: command_finished_requests::CommandFinishedRequests::default(
            ),
            terminal_bells: terminal_bells::TerminalBells::default(),
            terminal_signals: terminal_signals::TerminalSignals::default(),
            worker_live: worker_live::WorkerLiveMetrics::default(),
            transport_probes: BTreeMap::new(),
            pending_transport_probes: BTreeMap::new(),
            announced_pairings: VecDeque::new(),
            terminal_smoke_faults: crate::terminal::smoke_faults::TerminalSmokeFaults::default(),
        }
    }
}
