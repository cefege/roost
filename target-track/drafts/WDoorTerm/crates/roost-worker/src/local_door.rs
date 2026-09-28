//! Bounded loopback terminal sockets before and after an authenticated Hello.
//! This owner expires unauthenticated local sockets, caps live grant bindings,
//! and makes replaying one grant REPLACE its prior socket rather than multiply
//! sinks. Called by `local_terminal::sockets` for every loopback port; the peer
//! carrier has its own negotiation bound. Ports
//! `apps/worker/src/local-door/local-terminal-prehello.ts`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use roost_protocol::terminal_peer::peer::{
    TERMINAL_PEER_HELLO_DEADLINE_MS, TERMINAL_PEER_MAX_ESTABLISHED_PER_WORKER,
};
use tokio::runtime::Handle;
use tokio::task::AbortHandle;

/// What authenticating a socket against a grant did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedAdmission {
    pub admitted: bool,
    /// The socket this grant was bound to before, which the caller closes.
    pub replaced_socket_id: Option<String>,
}

/// Called with the id of a socket whose Hello deadline passed, from the timer
/// task and with no lock of this owner held.
pub type PreHelloTimeout = Arc<dyn Fn(&str) + Send + Sync>;

struct HelloTimer {
    id: u64,
    task: AbortHandle,
}

#[derive(Default)]
struct PreHelloState {
    timers: HashMap<String, HelloTimer>,
    authenticated_by_grant: HashMap<String, String>,
    grant_by_socket: HashMap<String, String>,
    next_timer: u64,
}

struct PreHelloShared {
    on_timeout: PreHelloTimeout,
    runtime: Handle,
    state: Mutex<PreHelloState>,
}

/// v2 `LocalTerminalPreHelloOwner`. Cheap to clone; clones share one table.
#[derive(Clone)]
pub struct LocalTerminalPreHelloOwner {
    shared: Arc<PreHelloShared>,
}

impl std::fmt::Debug for LocalTerminalPreHelloOwner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = formatter.debug_struct("LocalTerminalPreHelloOwner");
        if let Ok(state) = self.shared.state.try_lock() {
            debug
                .field("waiting", &state.timers.len())
                .field("authenticated", &state.authenticated_by_grant.len());
        }
        debug.finish_non_exhaustive()
    }
}

impl LocalTerminalPreHelloOwner {
    /// Deadline timers run on `runtime`; `on_timeout` closes what expired.
    pub fn new(on_timeout: PreHelloTimeout, runtime: Handle) -> Self {
        let shared = PreHelloShared { on_timeout, runtime, state: Mutex::default() };
        Self { shared: Arc::new(shared) }
    }

    /// Admit an unauthenticated socket and start its Hello deadline, or refuse
    /// it: already waiting, or the worker already holds its bound of them.
    pub fn admit(&self, socket_id: &str) -> bool {
        let mut state = self.lock();
        if state.timers.contains_key(socket_id) || state.timers.len() >= TERMINAL_PEER_MAX_ESTABLISHED_PER_WORKER {
            return false;
        }
        state.next_timer += 1;
        let id = state.next_timer;
        let weak = Arc::downgrade(&self.shared);
        let owned = socket_id.to_owned();
        let task = self.shared.runtime.spawn(async move {
            tokio::time::sleep(Duration::from_millis(TERMINAL_PEER_HELLO_DEADLINE_MS)).await;
            expire(&weak, &owned, id);
        });
        state.timers.insert(socket_id.to_owned(), HelloTimer { id, task: task.abort_handle() });
        true
    }

    /// Bind `socket_id` to `grant_id`. A grant already bound to another socket
    /// is re-pointed and that socket reported for closing; a new grant past the
    /// authenticated bound is refused.
    pub fn authenticate(&self, grant_id: &str, socket_id: &str) -> AuthenticatedAdmission {
        let mut state = self.lock();
        let previous = state.authenticated_by_grant.get(grant_id).cloned();
        if previous.is_none() && state.authenticated_by_grant.len() >= TERMINAL_PEER_MAX_ESTABLISHED_PER_WORKER {
            return AuthenticatedAdmission { admitted: false, replaced_socket_id: None };
        }
        clear_locked(&mut state, socket_id);
        state.authenticated_by_grant.insert(grant_id.to_owned(), socket_id.to_owned());
        state.grant_by_socket.insert(socket_id.to_owned(), grant_id.to_owned());
        AuthenticatedAdmission {
            admitted: true,
            replaced_socket_id: previous.filter(|prior| prior != socket_id),
        }
    }

    /// Stop a socket's Hello deadline.
    pub fn clear(&self, socket_id: &str) {
        clear_locked(&mut self.lock(), socket_id);
    }

    /// A socket is gone: its deadline stops and its grant binding is released
    /// when it still holds it.
    pub fn retire(&self, socket_id: &str) {
        let mut state = self.lock();
        clear_locked(&mut state, socket_id);
        let Some(grant_id) = state.grant_by_socket.remove(socket_id) else {
            return;
        };
        if state.authenticated_by_grant.get(&grant_id).is_some_and(|bound| bound == socket_id) {
            state.authenticated_by_grant.remove(&grant_id);
        }
    }

    pub fn dispose(&self) {
        let mut state = self.lock();
        for (_, timer) in state.timers.drain() {
            timer.task.abort();
        }
        state.authenticated_by_grant.clear();
        state.grant_by_socket.clear();
    }

    fn lock(&self) -> MutexGuard<'_, PreHelloState> {
        self.shared.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn clear_locked(state: &mut PreHelloState, socket_id: &str) {
    if let Some(timer) = state.timers.remove(socket_id) {
        timer.task.abort();
    }
}

/// The deadline passed. Only the timer that is still registered fires, so a
/// `clear` that raced the wake-up wins.
fn expire(shared: &Weak<PreHelloShared>, socket_id: &str, timer_id: u64) {
    let Some(shared) = shared.upgrade() else {
        return;
    };
    {
        let mut state = shared.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.timers.get(socket_id).is_none_or(|timer| timer.id != timer_id) {
            return;
        }
        state.timers.remove(socket_id);
    }
    tracing::info!(socket_id, "a local terminal socket reached its hello deadline");
    (shared.on_timeout)(socket_id);
}
