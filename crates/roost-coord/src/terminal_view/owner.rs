//! Coordinator memory of the workers that own their own terminal views: which
//! live connection advertised the capability, which sessions it owns, and the
//! membership it publishes back.
//!
//! Ported from `apps/coord/src/terminal/view/terminal-view-projection.ts`. This
//! is a read model only -- no minimizer, no stream, no geometry decision -- so
//! the coordinator can still answer presence, diagnostics and a respawn for a
//! session whose geometry is the worker's to own.
//!
//! WHY THE COORDINATOR KEEPS A ROW AT ALL. A respawn asks the view hub what
//! size a session is, and the honest answer for an owner-mode session is the
//! owner's own effective geometry. Re-deriving it from a second copy of the
//! membership would be a second answer that a worker-side change could drift
//! from, so the row stores the worker's answer verbatim.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

use roost_proto::WTerminalViewProjection;
use roost_protocol::viewport::{TerminalGeometry, minimum_terminal_geometry};
use roost_protocol::wire::{SessionId, WorkerFp};

use super::record::ViewInput;

/// Advertised in a worker's hello capabilities. The worker declares the same
/// literal for its own hello, and a hello without it is authoritative for its
/// fingerprint: a worker that downgraded would otherwise keep having its
/// sessions relayed to a build that no longer speaks the relay.
pub const TERMINAL_VIEW_OWNER_CAPABILITY: &str = "terminal-view-owner-v1";

/// One owner-mode session's published membership. `effective` and `stream_id`
/// are the worker's, never the coordinator's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerRow {
    /// The connection that published it.
    pub worker_fp: WorkerFp,
    /// Every viewer the worker admitted, parked ones included.
    pub viewers: Vec<ViewInput>,
    /// The size the worker is running the PTY at, or `None` when it published
    /// an empty viewer set.
    pub effective: Option<TerminalGeometry>,
    /// The stream the worker minted for this membership.
    pub stream_id: String,
}

impl OwnerRow {
    /// The per-device geometry this row projects for presence, minimized per
    /// device through the one shared primitive. Every viewer contributes,
    /// parked or not: presence answers who has the session open, which is a
    /// different question from who binds the PTY.
    #[must_use]
    pub fn viewer_geometry(&self) -> BTreeMap<String, TerminalGeometry> {
        let mut grouped: BTreeMap<&str, Vec<TerminalGeometry>> = BTreeMap::new();
        for viewer in &self.viewers {
            let geometry = TerminalGeometry {
                cols: viewer.cols,
                rows: viewer.rows,
            };
            grouped
                .entry(viewer.fingerprint.as_str())
                .or_default()
                .push(geometry);
        }
        let mut viewers = BTreeMap::new();
        for (fingerprint, geometries) in grouped {
            if let Ok(Some(geometry)) = minimum_terminal_geometry(&geometries) {
                viewers.insert(fingerprint.to_owned(), geometry);
            }
        }
        viewers
    }

    /// The devices holding a view of this session.
    #[must_use]
    pub fn viewer_fingerprints(&self) -> BTreeSet<String> {
        self.viewers
            .iter()
            .map(|viewer| viewer.fingerprint.clone())
            .collect()
    }
}

#[derive(Debug, Default)]
struct OwnerData {
    owners: HashSet<WorkerFp>,
    owner_sessions: HashMap<SessionId, WorkerFp>,
    rows: HashMap<SessionId, OwnerRow>,
}

/// Which workers own their own views, and what they published.
///
/// Interior mutability rather than a `Mutex` the hub holds, because an
/// [`OwnerRegistration`] has to be able to give its ownership back from a
/// socket-close callback that owns nothing but the registration.
#[derive(Debug, Default)]
pub struct OwnerIndex {
    data: Mutex<OwnerData>,
}

impl OwnerIndex {
    /// An index with no owner-mode workers.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn data(&self) -> MutexGuard<'_, OwnerData> {
        self.data.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Register an owner-mode connection, superseding any earlier one.
    pub fn register_owner(self: &Arc<Self>, worker_fp: &WorkerFp) -> OwnerRegistration {
        self.data().owners.insert(worker_fp.clone());
        tracing::info!(worker_fp = %worker_fp, "terminal view owner registered");
        OwnerRegistration {
            worker_fp: worker_fp.clone(),
            index: Arc::clone(self),
        }
    }

    /// A hello WITHOUT the capability is authoritative for its fingerprint.
    pub fn clear_owner(&self, worker_fp: &WorkerFp) {
        // The guard is released before the drop: this mutex is not reentrant,
        // and `drop_owner` takes it again.
        let owned = self.data().owners.contains(worker_fp);
        if owned {
            self.drop_owner(worker_fp);
        }
    }

    /// A connection is gone: its ownership and every row it published go.
    pub fn drop_owner(&self, worker_fp: &WorkerFp) {
        let mut data = self.data();
        if !data.owners.remove(worker_fp) {
            return;
        }
        data.rows.retain(|_, row| &row.worker_fp != worker_fp);
        data.owner_sessions.retain(|_, owner| owner != worker_fp);
        tracing::info!(worker_fp = %worker_fp, "terminal view owner released");
    }

    /// The owner-mode worker that owns this session's views, if one does.
    ///
    /// A session bound to a worker that is no longer registered stops being
    /// owned rather than staying bound to a dead connection.
    #[must_use]
    pub fn owner_for_session(&self, session_id: &SessionId) -> Option<WorkerFp> {
        let data = self.data();
        let owner = data.owner_sessions.get(session_id)?;
        data.owners.contains(owner).then(|| owner.clone())
    }

    /// Bind a session to the connection the route index resolved for it.
    ///
    /// Called from route reconciliation. It survives a later route-cache sweep
    /// on purpose: a view heartbeat that arrives between the sweep and the
    /// worker's exact snapshot would otherwise look ownerless, and an ownerless
    /// session is the one state where two components could each decide to
    /// minimize it.
    pub fn bind_session(&self, session_id: &SessionId, worker_fp: &WorkerFp) {
        let owned = self.data().owners.contains(worker_fp);
        if owned {
            self.data()
                .owner_sessions
                .insert(session_id.clone(), worker_fp.clone());
        }
    }

    /// Replace a session's membership wholesale, reporting whether the row moved.
    ///
    /// The worker publishes the full viewer list on every membership or
    /// effective-geometry change, including an empty list when the last viewer
    /// goes inactive, so no incremental merge exists. An empty list leaves a
    /// zero-viewer row rather than deleting it: diagnostics read that
    /// difference, and it is the only record that the session was watched.
    pub fn apply_projection(
        &self,
        worker_fp: &WorkerFp,
        projection: &WTerminalViewProjection,
    ) -> bool {
        let Ok(session_id) = SessionId::try_from(projection.session_id.clone()) else {
            tracing::warn!(
                worker_fp = %worker_fp,
                session_id = %projection.session_id,
                "a terminal view projection named a session id that is not one"
            );
            return false;
        };
        let viewers: Vec<ViewInput> = projection
            .viewers
            .iter()
            .map(|viewer| ViewInput {
                fingerprint: viewer.fingerprint.clone(),
                view_id: viewer.view_id.clone(),
                cols: viewer.cols,
                rows: viewer.rows,
                parked: viewer.parked,
                constrains: viewer.constrains,
            })
            .collect();
        let effective = (projection.effective_cols > 0 && projection.effective_rows > 0).then_some(
            TerminalGeometry {
                cols: projection.effective_cols,
                rows: projection.effective_rows,
            },
        );
        let row = OwnerRow {
            worker_fp: worker_fp.clone(),
            viewers,
            effective,
            stream_id: projection.stream_id.clone(),
        };
        let mut data = self.data();
        if !data.owners.contains(worker_fp) {
            return false;
        }
        if let Some(known) = data.owner_sessions.get(&session_id)
            && known != worker_fp
        {
            tracing::debug!(
                worker_fp = %worker_fp,
                %session_id,
                known_owner = %known,
                "a terminal view projection arrived from a worker that does not own the session"
            );
            return false;
        }
        let changed = data.rows.get(&session_id) != Some(&row);
        data.owner_sessions.insert(session_id.clone(), worker_fp.clone());
        data.rows.insert(session_id, row);
        changed
    }

    /// What a session's owner published, if it published anything.
    #[must_use]
    pub fn row(&self, session_id: &SessionId) -> Option<OwnerRow> {
        self.data().rows.get(session_id).cloned()
    }

    /// Forget a session's owner binding and its row.
    pub fn drop_projection(&self, session_id: &SessionId) {
        let mut data = self.data();
        data.rows.remove(session_id);
        data.owner_sessions.remove(session_id);
    }
}

/// A registration that releases only while it is still the current one, so a
/// reconnecting worker's delayed old socket cannot un-register its
/// replacement's ownership.
#[derive(Clone)]
pub struct OwnerRegistration {
    /// The worker this registration names.
    pub worker_fp: WorkerFp,
    index: Arc<OwnerIndex>,
}

impl OwnerRegistration {
    /// Give up this connection's ownership, if a newer one has not taken it.
    pub fn release(&self) {
        self.index.drop_owner(&self.worker_fp);
    }
}

impl fmt::Debug for OwnerRegistration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OwnerRegistration")
            .field("worker_fp", &self.worker_fp)
            .finish()
    }
}
