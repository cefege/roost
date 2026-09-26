//! The coordinator's whole path for a session whose worker owns its terminal
//! views: authorize a browser view or resync command exactly as the membership
//! machine would, then forward it instead of admitting local membership.
//!
//! Ported from `apps/coord/src/terminal/view/terminal-view-owner-relay.ts` and
//! `worker-send-terminal-view.ts`.
//!
//! IT HOLDS NO GEOMETRY AND MINTS NO STREAM. The worker is the only minimizer
//! for these sessions, so a relay that also computed an effective geometry here
//! would be the second minimizer `docs/FAILURE-INDEX.md` warns about ("route a
//! session the controller never minimized to the owning worker through the owner
//! relay ... audit what ELSE that path was the only caller of"). The only
//! geometry this file touches is what the owner published.
//!
//! WHY THE TRANSPORT IS A SEAM. The relay's frames belong on the
//! coordinator-worker wire union, and writing them is the worker link's job.
//! `TerminalViewHub::new()` must keep taking nothing, so the transport is
//! installed after the `WorkerRegistry` exists rather than at construction.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, RwLock};

use roost_proto::{TerminalResyncCommand, TerminalViewCommand, TerminalViewStatus};
use roost_protocol::wire::{SessionId, WorkerFp};

use super::record::validate_view_command;
use super::registry::SocketRecord;
use super::sink::PendingReply;

/// The authenticated identity a relayed command carries to the owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayIdentity {
    /// The coordinator-side socket the browser command arrived on.
    pub socket_id: String,
    /// The `${fingerprint}:${tab}` key that owns the handle.
    pub viewer_key: String,
    /// The verified device fingerprint.
    pub device_fingerprint: String,
}

/// The worker-link seam the relay writes through.
///
/// Each method returns whether this worker's transport admitted the frame: a
/// refusal is the relay's cue to tell the browser the terminal is unavailable
/// rather than to admit local state.
pub trait OwnerViewTransport: Send + Sync {
    /// Forward one authorized browser view command to the owning worker.
    fn relay_view(
        &self,
        worker_fp: &WorkerFp,
        identity: &RelayIdentity,
        command: &TerminalViewCommand,
    ) -> bool;

    /// Forward one authorized resync request to the owning worker.
    fn relay_resync(
        &self,
        worker_fp: &WorkerFp,
        identity: &RelayIdentity,
        command: &TerminalResyncCommand,
    ) -> bool;

    /// Tell the owning worker a relayed browser socket is gone, so its own
    /// registry can park that socket's views.
    fn socket_closed(&self, worker_fp: &WorkerFp, socket_id: &str) -> bool;
}

/// A transport with no worker link behind it.
///
/// Every relay is refused and the browser is told the terminal is unavailable.
/// Admitting local membership instead would make the coordinator a second
/// minimizer for a session whose worker already is one, which is the failure
/// this whole path exists to avoid.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoOwnerViewTransport;

impl OwnerViewTransport for NoOwnerViewTransport {
    fn relay_view(
        &self,
        worker_fp: &WorkerFp,
        _identity: &RelayIdentity,
        _command: &TerminalViewCommand,
    ) -> bool {
        tracing::warn!(
            worker_fp = %worker_fp,
            "a terminal view relay was refused: no worker-link transport is installed"
        );
        false
    }

    fn relay_resync(
        &self,
        worker_fp: &WorkerFp,
        _identity: &RelayIdentity,
        _command: &TerminalResyncCommand,
    ) -> bool {
        tracing::warn!(
            worker_fp = %worker_fp,
            "a terminal resync relay was refused: no worker-link transport is installed"
        );
        false
    }

    fn socket_closed(&self, worker_fp: &WorkerFp, _socket_id: &str) -> bool {
        tracing::warn!(
            worker_fp = %worker_fp,
            "a terminal view socket-close relay was refused: no worker-link transport is installed"
        );
        false
    }
}

/// What one relayed view command decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayOutcome {
    /// The owner this command went to or would have gone to.
    pub owner: WorkerFp,
    /// The answer owed to the browser socket, when the relay refused it.
    pub refusal: Option<PendingReply>,
}

/// One socket's relay bookkeeping.
#[derive(Debug, Default, Clone)]
struct RelaySocketState {
    /// Owner-mode workers this socket has been relayed to, so its close
    /// reaches them.
    workers: BTreeSet<WorkerFp>,
    /// Session to the view ids the owner has confirmed as members on this
    /// socket. Non-empty means the coordinator's screen replica must keep
    /// feeding that session's cells here; it is bookkeeping for fan-out, never
    /// geometry.
    watches: BTreeMap<SessionId, BTreeSet<String>>,
}

/// The owner-mode relay, and the transport its writes go through.
pub struct OwnerRelay {
    transport: RwLock<Arc<dyn OwnerViewTransport>>,
    sockets: Mutex<BTreeMap<String, RelaySocketState>>,
}

impl std::fmt::Debug for OwnerRelay {
    /// The transport is a trait object, so there is nothing of it to print; a
    /// log line needs how many sockets this relay is still holding for.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OwnerRelay")
            .field(
                "sockets",
                &self
                    .sockets
                    .lock()
                    .map_or(0, |sockets| sockets.len()),
            )
            .finish_non_exhaustive()
    }
}

impl Default for OwnerRelay {
    fn default() -> Self {
        Self::new()
    }
}

impl OwnerRelay {
    /// A relay with no transport installed.
    #[must_use]
    pub fn new() -> Self {
        Self {
            transport: RwLock::new(Arc::new(NoOwnerViewTransport)),
            sockets: Mutex::new(BTreeMap::new()),
        }
    }

    /// Install the worker-link transport, for the boot that has one.
    pub fn set_transport(&self, transport: Arc<dyn OwnerViewTransport>) {
        *self
            .transport
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = transport;
    }

    /// Authorize one view command and forward it to the session's owner.
    ///
    /// Returns `None` when the command was accepted and forwarded: the owner
    /// answers with its own view state, and a relay that also answered would be
    /// two sources of one decision.
    pub fn relay_view(
        &self,
        socket: &SocketRecord,
        owner: &WorkerFp,
        command: &TerminalViewCommand,
    ) -> Option<RelayOutcome> {
        let refusal = |status, reason: &str| RelayOutcome {
            owner: owner.clone(),
            refusal: Some(PendingReply::Command {
                socket_id: socket.id.clone(),
                view_id: command.view_id.clone(),
                session_id: command.session_id.clone(),
                revision: command.revision,
                active: command.active,
                status,
                reason: reason.to_owned(),
            }),
        };
        if let Some(reason) = validate_view_command(socket.viewer_key.as_deref(), command) {
            return Some(refusal(TerminalViewStatus::Rejected, reason));
        }
        if !socket.allows(&command.session_id) {
            return Some(refusal(
                TerminalViewStatus::Rejected,
                "terminal session is unavailable",
            ));
        }
        let Some(viewer_key) = socket.viewer_key.clone() else {
            return Some(refusal(
                TerminalViewStatus::Rejected,
                "terminal views require a tab-bound Sync socket",
            ));
        };
        let identity = RelayIdentity {
            socket_id: socket.id.clone(),
            viewer_key,
            device_fingerprint: socket.fingerprint.clone(),
        };
        if !self.transport().relay_view(owner, &identity, command) {
            return Some(refusal(
                TerminalViewStatus::Unavailable,
                "terminal worker is unavailable",
            ));
        }
        self.remember_worker(&socket.id, owner);
        tracing::debug!(
            worker_fp = %owner,
            socket_id = %socket.id,
            view_id = %command.view_id,
            revision = command.revision,
            "terminal view command relayed to its owner"
        );
        None
    }

    /// Forward one resync request.
    ///
    /// Resync carries no reply of its own in any path: a request the owner
    /// cannot honour is answered by the next view state.
    pub fn relay_resync(
        &self,
        socket: &SocketRecord,
        owner: &WorkerFp,
        command: &TerminalResyncCommand,
    ) -> bool {
        let Some(viewer_key) = socket.viewer_key.clone() else {
            return false;
        };
        if !socket.allows(&command.session_id) {
            return false;
        }
        let identity = RelayIdentity {
            socket_id: socket.id.clone(),
            viewer_key,
            device_fingerprint: socket.fingerprint.clone(),
        };
        if !self.transport().relay_resync(owner, &identity, command) {
            return false;
        }
        self.remember_worker(&socket.id, owner);
        true
    }

    /// A relayed socket is gone: tell every owner it reached.
    ///
    /// Fire and forget by design: the socket is already closed, and the owner's
    /// own lease sweep is the backstop when this never lands.
    pub fn close_socket(&self, socket_id: &str) {
        let Some(state) = self
            .sockets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(socket_id)
        else {
            return;
        };
        let transport = self.transport();
        for worker_fp in &state.workers {
            if !transport.socket_closed(worker_fp, socket_id) {
                tracing::warn!(
                    worker_fp = %worker_fp,
                    socket_id,
                    "a closed terminal view socket did not reach its owner; the owner's own lease sweep is the backstop"
                );
            }
        }
    }

    /// Record one owner view decision, reporting whether the socket still holds
    /// any confirmed view of the session and whether this decision is the one
    /// that newly attached it.
    pub fn track(
        &self,
        socket_id: &str,
        session_id: &SessionId,
        view_id: &str,
        member: bool,
    ) -> (bool, bool) {
        let mut sockets = self
            .sockets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let state = sockets.entry(socket_id.to_owned()).or_default();
        let Some(views) = state.watches.get_mut(session_id) else {
            if member {
                state
                    .watches
                    .entry(session_id.clone())
                    .or_default()
                    .insert(view_id.to_owned());
                return (true, true);
            }
            return (false, false);
        };
        if member {
            let attached = views.is_empty();
            views.insert(view_id.to_owned());
            return (true, attached);
        }
        views.remove(view_id);
        if !views.is_empty() {
            return (true, false);
        }
        state.watches.remove(session_id);
        (false, false)
    }

    fn transport(&self) -> Arc<dyn OwnerViewTransport> {
        Arc::clone(
            &self
                .transport
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    fn remember_worker(&self, socket_id: &str, worker_fp: &WorkerFp) {
        self.sockets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(socket_id.to_owned())
            .or_default()
            .workers
            .insert(worker_fp.clone());
    }
}
