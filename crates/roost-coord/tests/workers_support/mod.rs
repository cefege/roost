// The fixture every workers test drives: a migrated database with enrolled
// workers, a `CoordCore` over the no-op terminal seams, and a worker socket that
// records what it was handed.
//
// The no-op seams are the point of this file. `coord_core::seams` exists so a
// consumer can be exercised without the collaborator, and a fixture that
// installed a real byte hub by default would prove nothing about that: it would
// only prove the hub works. Every assertion in `tests/workers_*.rs` is about the
// workers domain.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use roost_coord::auth::principal::Principal;
use roost_coord::coord_core::worker_handle::{WorkerHandle, WorkerRegistry};
use roost_coord::coord_core::{
    Caller, CoordCore, CoordTerminal, ListenerTrust, TerminalViewLifecycle, WorkerRouteIndex,
};
use roost_coord::db::CoordDb;
use roost_coord::services::CoordServices;
use roost_coord::workers::registry::mark_generation_ready;
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use sqlx::AssertSqlSafe;

/// A valid worker fingerprint: 64 lowercase hex characters, which is what
/// `WorkerFp`'s brand accepts and therefore what the auth layer would mint.
pub const WORKER_FP: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// A second machine, for the tests that need more than one.
pub const OTHER_WORKER_FP: &str =
    "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

/// The account device an operator acts as.
pub const DEVICE_FP: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// A valid session id, for the rows a delete has to release.
pub const SESSION_ID: &str = "11111111-1111-4111-8111-111111111111";

/// Every downstream frame a socket was handed, in order.
#[derive(Debug, Clone, PartialEq)]
pub struct SentFrame(pub CoordWorkerDownstream);

/// A worker socket that records instead of writing.
#[derive(Debug, Default)]
pub struct RecordingSocket {
    frames: Mutex<Vec<SentFrame>>,
}

impl RecordingSocket {
    /// A socket with nothing recorded yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The send closure a `WorkerHandle` is built over.
    ///
    /// Non-zero and increasing, because zero is the transport's "dropped" answer
    /// and a test that cannot tell a refusal from a delivery is not testing the
    /// send path.
    #[must_use]
    pub fn sender(self: &Arc<Self>) -> Arc<dyn Fn(CoordWorkerDownstream) -> i64 + Send + Sync> {
        let socket = Arc::clone(self);
        Arc::new(move |frame| {
            let mut frames = socket
                .frames
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            frames.push(SentFrame(frame));
            frames.len() as i64
        })
    }

    /// Everything the socket was handed.
    #[must_use]
    pub fn frames(&self) -> Vec<SentFrame> {
        self.frames
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    /// How many frames the socket was handed.
    #[must_use]
    pub fn count(&self) -> usize {
        self.frames
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len()
    }
}

/// A coordinator over a temporary database, with no terminal collaborator.
pub struct WorkersFixture {
    /// The shared process state, and the only way into a handler.
    pub core: CoordCore,
    /// The services the core was built over, so a test can re-attach seams.
    services: Arc<CoordServices>,
    /// The database handle, for the rows a test asserts on directly.
    pub database: CoordDb,
    /// The scratch directory, removed when the fixture drops.
    root: PathBuf,
}

impl WorkersFixture {
    /// A fixture with the self-hosted tenant and no workers.
    pub async fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-workers-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let database = roost_coord::db::open(&root.join("coord.db"))
            .await
            .expect("a migrated database");
        roost_coord::auth::self_hosted_tenant::ensure_self_hosted_tenant(&database, 1_000)
            .await
            .expect("the self-hosted tenant");
        let services = Arc::new(CoordServices::new(database.clone()));
        Self {
            core: CoordCore::new(Arc::clone(&services)),
            services,
            database,
            root,
        }
    }

    /// Run one statement against the fixture's database.
    pub async fn exec(&self, sql: &str) {
        sqlx::query(AssertSqlSafe(sql))
            .execute(self.database.pool())
            .await
            .expect("the statement applies");
    }

    /// Enroll a worker row, as a redeemed bootstrap token would have.
    pub async fn enroll_worker(&self, worker_fp: &str, label: &str, registered_at_ms: i64) {
        self.exec(&format!(
            "INSERT INTO workers (fp, label, os, registered_at_ms, last_seen_ms, dashboard_id) \
             VALUES ('{worker_fp}', '{label}', 'linux', {registered_at_ms}, {registered_at_ms}, \
             (SELECT id FROM dashboards LIMIT 1))"
        ))
        .await;
    }

    /// Give the worker one open session, so a delete has something to release.
    pub async fn enroll_session(&self, worker_fp: &str, session_id: &str) {
        self.exec(&format!(
            "INSERT INTO sessions (id, worker_fp, channel, kind, cwd, status, created_at, \
             dashboard_id) VALUES ('{session_id}', '{worker_fp}', 1, 'shell', '/tmp', 'open', 1, \
             (SELECT id FROM dashboards LIMIT 1))"
        ))
        .await;
    }

    /// One integer out of a query.
    pub async fn scalar_i64(&self, sql: &str) -> i64 {
        sqlx::query_scalar::<_, i64>(AssertSqlSafe(sql))
            .fetch_one(self.database.pool())
            .await
            .expect("a scalar")
    }

    /// Claim a socket as the worker's current generation and make it routable.
    pub fn connect_worker(
        &self,
        worker_fp: &str,
        generation: &str,
        socket: &Arc<RecordingSocket>,
    ) -> Arc<WorkerHandle> {
        let handle = Arc::new(WorkerHandle::new(
            WorkerFp::try_from(worker_fp).expect("a fingerprint the brand accepts"),
            Some("epoch-1".to_owned()),
            generation.to_owned(),
            std::collections::BTreeSet::new(),
            socket.sender(),
        ));
        roost_coord::workers::registry::claim_generation(
            &self.core.services.buses,
            &self.core.services.workers,
            Arc::clone(&handle),
        );
        mark_generation_ready(
            &self.core.services.buses,
            &self.core.services.workers,
            &handle,
        );
        handle
    }

    /// Install real terminal collaborators instead of the no-op seams.
    ///
    /// A test that wants to see the workers domain reach its collaborators
    /// THROUGH the seams asks for them here; every other test runs with
    /// `CoordTerminal::none()`, which is the state a coordinator is in before
    /// the terminal hubs are built.
    pub fn attach_terminal_seams(
        &mut self,
        routes: Arc<dyn WorkerRouteIndex>,
        views: Arc<dyn TerminalViewLifecycle>,
    ) {
        self.core = CoordCore::with_terminal(
            Arc::clone(&self.services),
            CoordTerminal::new(routes, views),
        );
    }

    /// The registry, for a test that drives generations directly.
    #[must_use]
    pub fn registry(&self) -> &Arc<WorkerRegistry> {
        &self.core.services.workers
    }
}

impl Drop for WorkersFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The worker that is speaking.
#[must_use]
pub fn worker_caller(worker_fp: &str) -> Caller {
    caller_with(Principal::Worker {
        fingerprint: worker_fp.to_owned(),
        label: "test worker".to_owned(),
    })
}

/// The account device an operator acts as.
#[must_use]
pub fn device_caller() -> Caller {
    caller_with(Principal::AccountDevice {
        fingerprint: DEVICE_FP.to_owned(),
        label: "test device".to_owned(),
        account_id: "acct_self_hosted".to_owned(),
    })
}

fn caller_with(principal: Principal) -> Caller {
    Caller {
        principal,
        tab_id: None,
        remote_address: None,
        on_host: true,
        listener_trust: ListenerTrust::DirectLoopback,
    }
}
