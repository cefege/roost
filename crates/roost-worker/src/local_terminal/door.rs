//! Boot-time assembly of the direct terminal path: the one grant store and the
//! one socket owner, over the view owner, input routes and work budget the
//! coordinator link already shares, and the grant install/revoke the link
//! routes here. Built by `runtime::owners`; the door's router serves
//! `sockets()`, the link dispatcher reaches it as `LocalTerminalGrantPort`.
//! Ports the owner half of `apps/worker/src/boot/boot-local-terminal.ts`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use roost_proto::DLocalTerminalGrant;
use tokio::runtime::Handle;

use super::grants::LocalTerminalGrantStore;
use super::sockets::{LocalTerminalSockets, LocalTerminalSocketsDeps};
use crate::link_ports::LocalTerminalGrantPort;
use crate::session::lifecycle::SessionManager;
use crate::session::table::SessionTable;
use crate::terminal_input::{TerminalInputRouteOwner, TerminalInputWorkBudget};
use crate::terminal_view::TerminalViewOwner;

/// What the door's owners are built over. v2 `LocalTerminalDoorOptions` minus
/// the server's own configuration.
#[derive(Debug, Clone)]
pub struct LocalTerminalDoorDeps {
    pub manager: Arc<SessionManager>,
    pub sessions: Arc<SessionTable>,
    pub view: Arc<TerminalViewOwner>,
    pub routes: TerminalInputRouteOwner,
    pub work_budget: TerminalInputWorkBudget,
    pub worker_fingerprint: String,
    pub process_epoch: String,
    pub runtime: Handle,
}

/// v2 `LocalTerminalDoor.wiring` for the terminal owners.
#[derive(Debug)]
pub struct LocalTerminalDoor {
    grants: LocalTerminalGrantStore,
    sockets: Arc<LocalTerminalSockets>,
    routes: TerminalInputRouteOwner,
    work_budget: TerminalInputWorkBudget,
    worker_epoch: String,
    disposed: AtomicBool,
}

impl LocalTerminalDoor {
    #[must_use]
    pub fn new(deps: LocalTerminalDoorDeps) -> Arc<Self> {
        let grants = LocalTerminalGrantStore::new(deps.process_epoch.clone(), deps.runtime.clone());
        let sockets = LocalTerminalSockets::new(LocalTerminalSocketsDeps {
            manager: deps.manager,
            sessions: deps.sessions,
            grants: grants.clone(),
            view: deps.view,
            work_budget: deps.work_budget.clone(),
            routes: deps.routes.clone(),
            worker_fingerprint: deps.worker_fingerprint,
            worker_epoch: deps.process_epoch.clone(),
            runtime: deps.runtime,
        });
        tracing::info!(worker_epoch = %deps.process_epoch, "the local terminal door owners are built");
        Arc::new(Self {
            grants,
            sockets,
            routes: deps.routes,
            work_budget: deps.work_budget,
            worker_epoch: deps.process_epoch,
            disposed: AtomicBool::new(false),
        })
    }

    /// The socket owner the door's terminal route serves.
    pub fn sockets(&self) -> Arc<LocalTerminalSockets> {
        Arc::clone(&self.sockets)
    }

    /// The grant store the peer owner authorizes offers against.
    pub fn grants(&self) -> LocalTerminalGrantStore {
        self.grants.clone()
    }

    /// The process epoch every direct grant and probe is fenced to.
    pub fn worker_epoch(&self) -> &str {
        &self.worker_epoch
    }

    /// v2 `disposeDirect`: stop taking direct input, route claims and grants,
    /// and close every direct socket. Once; a retire and a shutdown may both
    /// ask.
    pub fn dispose(&self) {
        if self.disposed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.work_budget.dispose();
        self.routes.dispose();
        self.grants.dispose();
        self.sockets.dispose();
        tracing::info!("the direct terminal path was disposed");
    }
}

impl LocalTerminalGrantPort for LocalTerminalDoor {
    /// v2 `onLocalTerminalGrant`: a refused install answers with the reason.
    fn install_grant(&self, request: &DLocalTerminalGrant) -> Result<(), String> {
        self.grants.install(request).map(|_| ())
    }

    /// v2 `wiring.revokeDevice`: routes first, then grants, whose removal
    /// closes every socket the device held.
    fn revoke_device(&self, device_fingerprint: &str) {
        self.sockets.revoke_device(device_fingerprint);
    }
}
