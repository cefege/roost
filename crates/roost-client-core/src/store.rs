//! The store: everything the client knows, and nothing that belongs to a host.
//!
//! One struct, because a `ClientCore` with several independent globals is how two
//! `ClientCore`s in one test process became impossible. Every field here is
//! reachable from the core, greppable, and dropped with it.
//!
//! Depends on `roost_protocol` for the session projection, and on the terminal
//! and sync modules for their own state. It adds no rules of its own.

pub mod agent_launcher;
pub mod browse_entries;
pub mod browse_machine;
pub mod browse_paths;
pub mod browse_state;
pub mod folder_activity;
pub mod folder_name_validation;
pub(crate) mod frames_revision;
pub mod layout;
pub mod mutations;
pub mod navigation;
pub mod optimistic_spawn;
pub mod palette;
pub mod paths;
pub mod pending_close;
pub mod prefs;
pub mod root;
pub mod selectors;
pub mod shell_dialogs;
pub mod shell_intent;
pub mod sidebar;
pub mod spotlight;
pub mod sync_feeds;
pub mod sync_smoke;
pub mod terminal_nav_pad;
pub mod terminal_transport;
pub mod toasts;
pub mod transfers;
pub mod ui;
pub mod view_rotation;

pub use mutations::{PairRequest, delete_pair_request, delete_worker, replace_workers};
pub use navigation::{
    NavigationSearchAttention, NavigationSearchDocument, NavigationSources,
    project_navigation_search_documents,
};
pub use optimistic_spawn::{
    ClientOnlySession, SpawnSettlement, SpawnTicket, begin_optimistic_spawn, settle_spawn_rejected,
};
pub use paths::{ExactWorkerPaths, WorkerPaths};
pub use pending_close::CloseLabels;
pub use root::{BrowserAccessState, captured_generation_is_current};
pub use spotlight::Spotlight;
pub use toasts::{Toast, ToastId, ToastKind, ToastSource};
pub use transfers::{Transfer, TransferDirection, TransferState};
pub use view_rotation::{RotationView, SyncViewRotation};

/// The wire vocabulary a host needs to CALL these selectors and build the rows
/// the projections take.
///
/// Re-exported rather than left to a second dependency edge: `SessionPlane`
/// already hands out `&Session` and `SessionMap` publicly
/// (`sessions.rs:50-56`), so the types this module traffics in are already part of
/// this crate's API. A host — and this crate's own behaviour tests — should not
/// need a `roost-protocol` dependency to name a row.
pub use roost_protocol::wire::{
    ChannelId, McpRelay, Session, SessionId, SessionKind, SessionMap, SessionStatus, Worker,
    WorkerFp, WorkerOs, WorkspaceId,
};

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use roost_protocol::wire::{Task, Workspace};

use crate::search::FindMatch;
use crate::sessions::SessionPlane;
use crate::store::optimistic_spawn::SpawnLedger;
use crate::store::pending_close::PendingCloses;
use crate::store::prefs::Prefs;
use crate::store::sync_feeds::{
    PendingTransportProbe, PresenceNotice, ProbeTelemetry, RoutableAssembly,
};
use crate::store::toasts::ToastStack;
use crate::store::transfers::TransferStack;
use crate::store::ui::UiState;
use crate::sync::SyncState;
use crate::sync::inbound::{AuditEntry, SessionViewer};
use crate::terminal::session::TerminalSession;
use crate::terminal::token::TerminalToken;
use crate::terminal::{InputPhase, InputRouter, RouteRegistry};

/// The client's whole state.
#[derive(Debug)]
pub struct Store {
    /// The projected session plane. Folded by `roost_protocol`, never here.
    pub sessions: SessionPlane,
    /// The Sync socket, its domains, and the recovery cursor.
    pub sync: SyncState,
    /// The tab this client presents. A v2 socket without one is READ-ONLY
    /// (`protocol/spec/sync.md:25`), so an empty tab is a real state the host has
    /// to resolve, not a placeholder to paper over.
    pub tab_id: String,
    /// Whether the pre-hydration store is ready. Until this is true, live
    /// application frames are retained rather than applied.
    pub hydrated: bool,
    /// The account this credential belongs to, once a host has established it.
    pub account_id: Option<String>,
    /// One replica per session that has a terminal pane.
    pub terminal: BTreeMap<String, TerminalSession>,
    /// One peer-negotiation machine per worker this document has wanted a
    /// direct carrier for. Fed by `handle_terminal` and by the host's own
    /// observations; the elected routes themselves stay in `routes`, because a
    /// route is a value and a machine is a negotiation.
    pub direct: crate::client::carriers::CarrierLane,
    /// Direct carriers, elected routes, and staged candidates.
    pub routes: RouteRegistry,
    /// Sessions re-registering their views on Sync because an elected direct
    /// route was lost, and the ids they minted for the move have not all come
    /// back yet.
    ///
    /// While an entry exists, every Sync publication for that session is
    /// suppressed: the id the record holds belonged to a carrier that is gone, and
    /// publishing it to the coordinator would put a lease on an id the direct
    /// worker is still holding in its own table.
    pub pending_sync_view_rotation: BTreeMap<String, SyncViewRotation>,
    /// Admitted input batches and their outcomes.
    pub input: InputRouter,
    /// Fenced find results by session. Retained rather than streamed: a result
    /// row carries its own grid epoch, and a list rebuilt from the replica would
    /// re-point old results at whatever the grid has become.
    pub find_results: BTreeMap<String, Vec<FindMatch>>,
    /// The worker registry, by fingerprint. Mutated only through
    /// `store::mutations`, which is where the bootstrap list lands.
    pub workers: BTreeMap<String, Worker>,
    /// Advances whenever this browser's authenticated resources become obsolete.
    /// Work that captured an older generation may no longer write.
    pub auth_generation: u64,
    /// Whether this browser's device key is trusted. `checking` until the
    /// protected sessions snapshot publishes, which is the closed default.
    pub browser_access_state: BrowserAccessState,

    /// The notification cards, their identity, and their deadlines. Mutated
    /// only through `store::toasts`.
    pub toasts: ToastStack,
    /// The upload/download cards. Mutated only through `store::transfers`.
    pub transfers: TransferStack,
    /// Which session is floated, and how many panes the deck shows. Mutated
    /// only through `store::spotlight`.
    pub spotlight: Spotlight,
    /// Chrome state that is not a session: the sidebar, the drawer, the
    /// folder view. Mutated only through `store::ui`.
    pub ui: UiState,
    /// The terminal key pad's open state and its disarm count. Mutated only
    /// through `store::terminal_nav_pad`.
    pub terminal_nav_pad: crate::store::terminal_nav_pad::TerminalNavPad,
    /// The sidebar's cursor and visit memory. Mutated only through `store::sidebar`.
    pub sidebar: crate::store::sidebar::SidebarState,
    /// The rename and queue-task dialogs. Mutated only through `store::shell_intent`.
    pub shell_dialogs: crate::store::shell_dialogs::ShellDialogs,
    /// The deck's stored arrangements and pending route. Mutated only through `deck::intent`.
    pub deck: crate::deck::DeckState,
    /// The coordinator's build and public URL, once `AuthCoordIdentity`
    /// answered (v2 `rootStore.coord_identity`).
    pub coord_identity: Option<crate::store::root::CoordIdentity>,
    /// Per-device preferences. Loaded once at boot and persisted by the same
    /// functions that change it, so a stored value and its in-memory value
    /// cannot disagree. Mutated only through `store::prefs`.
    pub prefs: Prefs,

    /// Per-session agent status, projected through the SHARED ordering
    /// predicate in `roost_protocol::wire::agent_status::order` rather than
    /// one restated here. v2 had a second copy in the web store that could
    /// disagree with the coordinator's, and a client showing a stale status
    /// has nothing to contradict it.
    pub agent_status: crate::client::agents::AgentStatusProjection,
    /// Which occupant and epoch this browser has already seen, so a report
    /// that arrives after the one it supersedes is dropped rather than
    /// applied and then re-dropped by the projection.
    pub agent_seen: crate::client::agents::AgentSeenLedger,
    /// Whether the acknowledgement ledger owes the host a write.
    ///
    /// Latched by the acknowledgement paths and cleared by the sweep that emits
    /// `Effect::PersistAgentSeen`, which is what makes a burst of
    /// acknowledgements one write. Initialise it to `false` in `Store::new`.
    pub agent_seen_dirty: bool,
    /// Machine-scoped browse state. Keyed by machine, not by path: two
    /// machines can hold the same folder path and a path is not an identity.
    pub browse: crate::store::browse_state::BrowseState,
    /// The fleet-wide content search: one outstanding page, a client-minted
    /// search id, and the reconciliation a cursor-paged answer needs.
    pub global_search: crate::client::global_search::GlobalSearchController,
    /// In-flight optimistic spawns, and the tombstones of the ones a user
    /// retracted. Mutated only through `store::optimistic_spawn`.
    pub spawns: SpawnLedger,
    /// Sessions whose close is waiting out its undo window. Mutated only
    /// through `store::pending_close`.
    pub pending_closes: PendingCloses,
    /// The default-agent launch configuration the coordinator answered with.
    /// Mutated only through `store::agent_launcher`; `None`-commanded until a
    /// coordinator has answered, so a pre-answer spawn launches nothing.
    pub agent_launcher: crate::store::agent_launcher::AgentLauncherState,
    /// The MCP relays the coordinator published. Mutated only through
    /// `store::mutations`.
    pub mcp_relays: BTreeMap<String, McpRelay>,
    /// Pair requests this browser is waiting on, keyed by their ephemeral id.
    /// Mutated only through `store::mutations`.
    pub pair_requests: BTreeMap<String, PairRequest>,

    // ---- Sync feed slices: written only by the folds under `handle_sync/` ---
    /// Workspaces by id (v2 `_handleWorkspacesDelta`).
    pub workspaces: BTreeMap<String, Workspace>,
    /// Tasks by id (v2 `_handleTasksDelta`).
    pub tasks: BTreeMap<String, Task>,
    /// The fingerprints the coordinator can route to RIGHT NOW, or `None`
    /// before the first set, which means "ask heartbeat freshness instead"
    /// (`sync-routable.ts:14-19`).
    pub routable_worker_fps: Option<BTreeSet<String>>,
    /// The chunked routable seed still being assembled on this socket.
    pub routable_assembly: RoutableAssembly,
    /// Coordinator-parsed OSC titles, by session.
    pub terminal_titles: BTreeMap<String, String>,
    /// Coordinator-stamped last activity, by session, in milliseconds.
    pub last_activity_ms: BTreeMap<String, i64>,
    /// Who is looking at each session, replaced per `viewers` notice.
    pub session_viewers: BTreeMap<String, Vec<SessionViewer>>,
    /// Opaque presence notices for the panes' presence handlers, oldest first.
    pub presence_notices: VecDeque<PresenceNotice>,
    /// Live audit rows, newest first, deduplicated by id and bounded.
    pub audit_rows: VecDeque<AuditEntry>,
    /// UI commands for the UI bridge, oldest first.
    pub ui_commands: VecDeque<roost_proto::UiCommandFrame>,
    /// The newest answered control probe per worker.
    pub transport_probes: BTreeMap<String, ProbeTelemetry>,
    /// Control probes sent and not yet answered, by request id.
    pub pending_transport_probes: BTreeMap<String, PendingTransportProbe>,
    /// Pairings already announced, oldest first, so one pairing toasts once.
    pub announced_pairings: VecDeque<String>,
    /// Smoke-armed frame faults; empty unless a smoke build armed one.
    pub terminal_smoke_faults: crate::terminal::smoke_faults::TerminalSmokeFaults,

    /// The next Connect call id, so a result is correlated with its call.
    next_call_id: u64,
    /// The next staging attempt id. Monotonic, so a slow fold cannot overwrite a
    /// newer one.
    pub next_attempt_id: u64,
    /// How many mutations this store has accepted.
    ///
    /// A host reads it after every `handle` and repaints the chrome when it
    /// moved. A COUNTER rather than a flag, for two reasons that are the same
    /// reason: "has anything changed since I looked" is a comparison, and a
    /// comparison cannot be lost by a reader that looked twice or by a writer
    /// that fired twice between two reads. A boolean has to be cleared, and
    /// "who clears it, and when" is a second question with a second wrong
    /// answer — a reader that clears what it has not read yet loses a repaint,
    /// and one that never clears repaints per event.
    ///
    /// It moves where a mutation LANDS, not once per `handle`. A keepalive and
    /// a cell frame the fold refused both reach `handle` and change nothing,
    /// and a host that repaints per control frame turns a busy socket into a
    /// busy main thread. `note_change` is the only thing that moves it.
    revision: u64,
    /// Moves when a replica's painted frame moves; see `store::frames_revision`
    /// for why it is a second counter.
    frames_revision: u64,
}

impl Store {
    /// An empty store, presenting `tab_id` on every dial.
    pub fn new(sync: SyncState, tab_id: impl Into<String>) -> Self {
        Self {
            revision: 0,
            frames_revision: 0,
            sessions: SessionPlane::new(),
            sync,
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
            // The four slices a sibling added to this struct; their declarations
            // are here and their zero-arg constructors are wired here, because a
            // field with no initialiser does not compile and this file is the one
            // that owns the constructor.
            agent_status: crate::client::agents::AgentStatusProjection::new(),
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
            transport_probes: BTreeMap::new(),
            pending_transport_probes: BTreeMap::new(),
            announced_pairings: VecDeque::new(),
            terminal_smoke_faults: crate::terminal::smoke_faults::TerminalSmokeFaults::default(),
        }
    }

    /// A session's replica, creating it on first use.
    ///
    /// Created lazily because a session with no pane has no replica to keep, and
    /// a pre-created one per session in a large account is a per-session grid the
    /// client never needed to hold.
    pub fn terminal_mut(&mut self, session_id: &str, worker_fp: &str) -> &mut TerminalSession {
        self.terminal
            .entry(session_id.to_string())
            .or_insert_with(|| TerminalSession::new(session_id, worker_fp))
    }

    /// A session's replica, if one exists.
    pub fn terminal(&self, session_id: &str) -> Option<&TerminalSession> {
        self.terminal.get(session_id)
    }

    /// A session's replica, mutable, if one exists.
    pub fn terminal_mut_if_present(&mut self, session_id: &str) -> Option<&mut TerminalSession> {
        self.terminal.get_mut(session_id)
    }

    /// How many mutations this store has accepted.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Record that a mutation landed here.
    ///
    /// `pub(crate)` because the only writers are the `handle_*` functions in
    /// this crate, and a host that wrote the store itself would be a second
    /// state machine with a revision nobody increments.
    pub(crate) fn note_change(&mut self) {
        self.revision += 1;
    }

    /// The generation the client is currently fenced to, from the live socket.
    pub fn sync_terminal_token(&self) -> Option<TerminalToken> {
        self.sync.terminal_token()
    }

    /// Whether a session's replica is holding a complete baseline.
    pub fn is_paintable(&self, session_id: &str) -> bool {
        self.terminal
            .get(session_id)
            .is_some_and(|session| session.baseline_ready())
    }

    /// The next Connect call id. Monotonic, so a late result is still correlatable.
    pub fn next_call_id(&mut self) -> u64 {
        let id = self.next_call_id;
        self.next_call_id += 1;
        id
    }

    /// Drop every session's replica, its find results, and its input lane.
    ///
    /// One call, because the three are fenced to each other: a replica with no
    /// views has nobody to resync for, and an input lane with no replica has
    /// nothing to be the route to. Cleaning two and leaving the third is how a
    /// session id outlives its own teardown.
    pub fn forget_session(&mut self, session_id: &str) {
        self.terminal.remove(session_id);
        self.find_results.remove(session_id);
        self.input.set_phase(session_id, InputPhase::Closed);
        // A card whose button reveals a session that no longer exists is a button
        // that navigates to nothing, so the cards go with the session. The inner
        // form does NOT note the change: this method has never bumped, and the
        // caller that owns the teardown notes it once for all four writes.
        crate::store::toasts::remove_toasts_for_session(&mut self.toasts, session_id);
    }
}
