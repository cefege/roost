//! Bounded, browser-bound continuation cursors, active-search selections, and
//! cancellation ordering for install-wide terminal search.
//!
//! Ports `apps/coord/src/search/global-search-cursors.ts`
//! (`GlobalSearchCursorOwner`). One owner per process lives on
//! `services.search`; `search::rpc_search` and `search::cancel` share it. Expiry
//! and per-device eviction are lazy, so cursors need no background timer. v2's
//! `onCancel` listeners are one `CancellationToken` per active search here.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use roost_protocol::terminal_search::{
    GLOBAL_TERMINAL_SEARCH_CURSOR_TTL_MS, GLOBAL_TERMINAL_SEARCH_MAX_CURSORS_PER_DEVICE,
};
use tokio_util::sync::CancellationToken;

use crate::search::cursor_types::{
    CursorIssueRefusal, GlobalSearchAdmission, GlobalSearchCancellationPreparation,
    GlobalSearchCursorBinding, GlobalSearchCursorIssue, GlobalSearchCursorProgress,
    GlobalSearchIdentity, GlobalSearchSessionPosition,
};

/// Live cancel tombstones one device may hold before its oldest is evicted.
pub const GLOBAL_SEARCH_MAX_CANCEL_TOMBSTONES_PER_DEVICE: usize = 128;
const GLOBAL_SEARCH_MAX_CANCEL_TOMBSTONES: usize = 4_096;
/// Active searches one device may run at once.
pub const GLOBAL_SEARCH_MAX_ACTIVE_PER_DEVICE: usize = 4;
const GLOBAL_SEARCH_MAX_ACTIVE: usize = 1_024;

/// The wall clock the owner expires cursors and tombstones by, in ms.
pub type CursorClock = Arc<dyn Fn() -> i64 + Send + Sync>;
/// The source of opaque cursor tokens.
pub type CursorTokenSource = Arc<dyn Fn() -> Result<String, String> + Send + Sync>;

#[derive(Debug)]
struct CursorRecord {
    binding: GlobalSearchCursorBinding,
    sessions: Vec<GlobalSearchSessionPosition>,
    eligible_sessions: usize,
    searched_session_ids: Vec<String>,
    created_order: u64,
    expires_at_ms: i64,
}

#[derive(Debug)]
struct ActiveSearchRecord {
    selected_sessions: Vec<GlobalSearchSessionPosition>,
    cancellation: CancellationToken,
}

#[derive(Debug)]
struct CancellationTombstone {
    identity: GlobalSearchIdentity,
    expires_at_ms: i64,
}

#[derive(Debug, Default)]
struct CursorState {
    cursors: HashMap<String, CursorRecord>,
    active: HashMap<GlobalSearchIdentity, ActiveSearchRecord>,
    /// Insertion order is eviction order, as v2's `Map` iteration.
    tombstones: VecDeque<CancellationTombstone>,
    created_order: u64,
}

/// Per-process owner of continuation cursors, active selections, and cancel
/// tombstones.
pub struct GlobalSearchCursorOwner {
    now_ms: CursorClock,
    new_token: CursorTokenSource,
    state: Mutex<CursorState>,
}

impl std::fmt::Debug for GlobalSearchCursorOwner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state();
        formatter
            .debug_struct("GlobalSearchCursorOwner")
            .field("cursors", &state.cursors.len())
            .field("active", &state.active.len())
            .field("tombstones", &state.tombstones.len())
            .finish()
    }
}

impl Default for GlobalSearchCursorOwner {
    fn default() -> Self {
        Self::new()
    }
}

impl GlobalSearchCursorOwner {
    /// An owner on the wall clock, minting v4-shaped tokens from the kernel CSPRNG.
    #[must_use]
    pub fn new() -> Self {
        Self::with_sources(
            Arc::new(crate::serve::now_ms),
            Arc::new(|| {
                crate::coord_core::ids::draw::<16>()
                    .map(crate::coord_core::ids::render_v4)
                    .map_err(|error| error.to_string())
            }),
        )
    }

    /// An owner whose clock and token source the caller supplies.
    #[must_use]
    pub fn with_sources(now_ms: CursorClock, new_token: CursorTokenSource) -> Self {
        Self {
            now_ms,
            new_token,
            state: Mutex::new(CursorState::default()),
        }
    }

    /// Admit a search, unless it was already cancelled, is already running,
    /// or its device (or the process) is at capacity.
    pub fn begin_search(&self, identity: &GlobalSearchIdentity) -> GlobalSearchAdmission {
        let mut state = self.purged();
        if state
            .tombstones
            .iter()
            .any(|stone| &stone.identity == identity)
        {
            return GlobalSearchAdmission::Cancelled;
        }
        if state.active.contains_key(identity) {
            return GlobalSearchAdmission::Duplicate;
        }
        let active_for_device = state
            .active
            .keys()
            .filter(|active| active.device_fingerprint == identity.device_fingerprint)
            .count();
        if active_for_device >= GLOBAL_SEARCH_MAX_ACTIVE_PER_DEVICE
            || state.active.len() >= GLOBAL_SEARCH_MAX_ACTIVE
        {
            return GlobalSearchAdmission::Capacity;
        }
        state.active.insert(
            identity.clone(),
            ActiveSearchRecord {
                selected_sessions: Vec::new(),
                cancellation: CancellationToken::new(),
            },
        );
        GlobalSearchAdmission::Started
    }

    /// Record the sessions an active search selected, so a cancel reaches
    /// them. False when the search was cancelled or is no longer active.
    pub fn select_sessions(
        &self,
        identity: &GlobalSearchIdentity,
        sessions: &[GlobalSearchSessionPosition],
    ) -> bool {
        let mut state = self.purged();
        if state
            .tombstones
            .iter()
            .any(|stone| &stone.identity == identity)
        {
            return false;
        }
        let Some(active) = state.active.get_mut(identity) else {
            return false;
        };
        active.selected_sessions = sessions.to_vec();
        true
    }

    /// The token a completed cancellation fires; already fired when the
    /// search is not active, exactly as v2 calls a late listener at once.
    #[must_use]
    pub fn on_cancel(&self, identity: &GlobalSearchIdentity) -> CancellationToken {
        let state = self.state();
        if let Some(active) = state.active.get(identity) {
            return active.cancellation.clone();
        }
        let fired = CancellationToken::new();
        fired.cancel();
        fired
    }

    /// Whether a live tombstone retires this identity.
    #[must_use]
    pub fn is_cancelled(&self, identity: &GlobalSearchIdentity) -> bool {
        let state = self.purged();
        state
            .tombstones
            .iter()
            .any(|stone| &stone.identity == identity)
    }

    /// Drop the active record of a search that ended.
    pub fn finish_search(&self, identity: &GlobalSearchIdentity) {
        self.state().active.remove(identity);
    }

    /// Issue an opaque cursor over a page's continuations.
    pub fn issue_cursor(
        &self,
        issue: GlobalSearchCursorIssue,
    ) -> Result<String, CursorIssueRefusal> {
        let mut state = self.purged();
        let GlobalSearchCursorIssue {
            binding,
            continuations,
            eligible_sessions,
            searched_session_ids,
        } = issue;
        if continuations.is_empty()
            || continuations.len() > binding.limits.max_sessions
            || eligible_sessions < continuations.len()
            || searched_session_ids.len() > eligible_sessions
        {
            return Err(CursorIssueRefusal::UnboundedProgress);
        }
        let mut seen: HashSet<&str> = HashSet::new();
        for continuation in &continuations {
            let position = &continuation.position;
            if position.grid_epoch.is_empty() && position.before_row.is_some() {
                return Err(CursorIssueRefusal::RowWithoutEpoch);
            }
            if !seen.insert(position.session_id.as_str()) {
                return Err(CursorIssueRefusal::DuplicateSession);
            }
            // A page that actually scanned a session must leave it strictly
            // closer to the history floor; an unchanged row would page over
            // the same rows forever, holding a worker lane per page.
            if continuation.searched
                && let (Some(requested), Some(resumed)) =
                    (continuation.requested_before_row, position.before_row)
                && resumed >= requested
            {
                return Err(CursorIssueRefusal::SearchedSessionDidNotAdvance);
            }
        }
        Self::evict_oldest_device_cursor(&mut state, &binding.identity.device_fingerprint);
        let token = (self.new_token)().map_err(CursorIssueRefusal::TokenUnavailable)?;
        state.created_order += 1;
        let record = CursorRecord {
            binding,
            sessions: continuations
                .into_iter()
                .map(|entry| entry.position)
                .collect(),
            eligible_sessions,
            searched_session_ids,
            created_order: state.created_order,
            expires_at_ms: (self.now_ms)() + i64::from(GLOBAL_TERMINAL_SEARCH_CURSOR_TTL_MS),
        };
        state.cursors.insert(token.clone(), record);
        Ok(token)
    }

    /// Claim a cursor once, under exactly the binding it was issued for.
    pub fn claim_cursor(
        &self,
        token: &str,
        binding: &GlobalSearchCursorBinding,
    ) -> Option<GlobalSearchCursorProgress> {
        let mut state = self.purged();
        if state.cursors.get(token)?.binding != *binding {
            return None;
        }
        let cursor = state.cursors.remove(token)?;
        Some(GlobalSearchCursorProgress {
            sessions: cursor.sessions,
            eligible_sessions: cursor.eligible_sessions,
            searched_session_ids: cursor.searched_session_ids,
        })
    }

    /// Retire a search BEFORE any worker is told: tombstone it, drop its
    /// cursors, and hand back its selection. Nothing to dispatch when a
    /// tombstone already exists.
    pub fn prepare_cancellation(
        &self,
        identity: &GlobalSearchIdentity,
    ) -> GlobalSearchCancellationPreparation {
        let mut state = self.purged();
        if state
            .tombstones
            .iter()
            .any(|stone| &stone.identity == identity)
        {
            return GlobalSearchCancellationPreparation {
                should_dispatch: false,
                selected_sessions: Vec::new(),
            };
        }
        let mut selected: Vec<GlobalSearchSessionPosition> = Vec::new();
        if let Some(active) = state.active.get(identity) {
            for session in &active.selected_sessions {
                if let Some(existing) = selected
                    .iter_mut()
                    .find(|kept| kept.session_id == session.session_id)
                {
                    *existing = session.clone();
                } else {
                    selected.push(session.clone());
                }
            }
        }
        state
            .cursors
            .retain(|_, cursor| cursor.binding.identity != *identity);
        Self::evict_cancellation_capacity(&mut state, &identity.device_fingerprint);
        state.tombstones.push_back(CancellationTombstone {
            identity: identity.clone(),
            expires_at_ms: (self.now_ms)() + i64::from(GLOBAL_TERMINAL_SEARCH_CURSOR_TTL_MS),
        });
        GlobalSearchCancellationPreparation {
            should_dispatch: true,
            selected_sessions: selected,
        }
    }

    /// Release an in-flight search's waiters, only after the cancels were sent.
    pub fn complete_cancellation(&self, identity: &GlobalSearchIdentity) {
        let removed = self.state().active.remove(identity);
        if let Some(active) = removed {
            tracing::info!(
                search_id = %identity.search_id,
                device = %identity.device_fingerprint,
                "global_search_cancelled"
            );
            active.cancellation.cancel();
        }
    }

    fn state(&self) -> MutexGuard<'_, CursorState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn purged(&self) -> MutexGuard<'_, CursorState> {
        let now = (self.now_ms)();
        let mut state = self.state();
        state.cursors.retain(|_, cursor| cursor.expires_at_ms > now);
        state.tombstones.retain(|stone| stone.expires_at_ms > now);
        state
    }

    fn evict_oldest_device_cursor(state: &mut CursorState, device_fingerprint: &str) {
        let mut device_cursors: Vec<(u64, String)> = state
            .cursors
            .iter()
            .filter(|(_, cursor)| cursor.binding.identity.device_fingerprint == device_fingerprint)
            .map(|(token, cursor)| (cursor.created_order, token.clone()))
            .collect();
        device_cursors.sort_unstable();
        let cap = GLOBAL_TERMINAL_SEARCH_MAX_CURSORS_PER_DEVICE as usize;
        let excess = (device_cursors.len() + 1).saturating_sub(cap);
        for (_, token) in device_cursors.into_iter().take(excess) {
            state.cursors.remove(&token);
        }
    }

    fn evict_cancellation_capacity(state: &mut CursorState, device_fingerprint: &str) {
        let mut device_count = state
            .tombstones
            .iter()
            .filter(|stone| stone.identity.device_fingerprint == device_fingerprint)
            .count();
        while device_count >= GLOBAL_SEARCH_MAX_CANCEL_TOMBSTONES_PER_DEVICE {
            let Some(oldest) = state
                .tombstones
                .iter()
                .position(|stone| stone.identity.device_fingerprint == device_fingerprint)
            else {
                break;
            };
            state.tombstones.remove(oldest);
            device_count -= 1;
        }
        while state.tombstones.len() >= GLOBAL_SEARCH_MAX_CANCEL_TOMBSTONES {
            state.tombstones.pop_front();
        }
    }
}
