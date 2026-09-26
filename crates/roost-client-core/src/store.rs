//! The store: everything the client knows, and nothing that belongs to a host.
//!
//! One struct, because a `ClientCore` with several independent globals is how two
//! `ClientCore`s in one test process became impossible. Every field here is
//! reachable from the core, greppable, and dropped with it.
//!
//! Depends on `roost_protocol` for the session projection, and on the terminal
//! and sync modules for their own state. It adds no rules of its own.

use std::collections::BTreeMap;

use crate::search::FindMatch;
use crate::sessions::SessionPlane;
use crate::sync::SyncState;
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
    /// Direct carriers, elected routes, and staged candidates.
    pub routes: RouteRegistry,
    /// Admitted input batches and their outcomes.
    pub input: InputRouter,
    /// Fenced find results by session. Retained rather than streamed: a result
    /// row carries its own grid epoch, and a list rebuilt from the replica would
    /// re-point old results at whatever the grid has become.
    pub find_results: BTreeMap<String, Vec<FindMatch>>,
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
}

impl Store {
    /// An empty store, presenting `tab_id` on every dial.
    pub fn new(sync: SyncState, tab_id: impl Into<String>) -> Self {
        Self {
            revision: 0,
            sessions: SessionPlane::new(),
            sync,
            tab_id: tab_id.into(),
            hydrated: false,
            account_id: None,
            terminal: BTreeMap::new(),
            routes: RouteRegistry::new(),
            input: InputRouter::new(),
            find_results: BTreeMap::new(),
            next_call_id: 1,
            next_attempt_id: 1,
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
    }
}
