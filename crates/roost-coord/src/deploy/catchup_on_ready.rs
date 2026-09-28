//! The worker-lifecycle observer that runs the catch-up deploy each time a
//! worker generation crosses its snapshot barrier and becomes routable.
//! Registered on `CoordServices::worker_lifecycle` by `services.rs`; depends on
//! `deploy::{catchup,start}`. Ports the `startCatchUpDeployOnAttach` call of
//! v2 `main.ts` onWorkerConnected (apps/coord/src/deploy/worker-catchup-deploy.ts).
//!
//! v2's same hook first resumes the worker's signed Windows update deploys; that
//! updater is not carried by this coordinator, so the catch-up is the whole hook.

use std::sync::Arc;

use roost_host::{ProcessEnv, build_identity};

use crate::coord_core::worker_handle::WorkerHandle;
use crate::coord_core::worker_lifecycle::WorkerLifecycleObserver;
use crate::db::CoordDb;
use crate::deploy::DeployRuntime;
use crate::deploy::catchup::CatchUpDeployOptions;
use crate::deploy::start::start_deploy;

/// Starts the catch-up for a worker that just became routable.
#[derive(Debug)]
pub struct CatchUpOnReady {
    runtime: DeployRuntime,
    database: CoordDb,
}

impl CatchUpOnReady {
    /// An observer over the process's one deploy runtime and database.
    #[must_use]
    pub fn new(runtime: DeployRuntime, database: CoordDb) -> Self {
        Self { runtime, database }
    }
}

impl WorkerLifecycleObserver for CatchUpOnReady {
    /// Detached, as v2's `void`-ed hook is: the link's read loop must not wait
    /// on a database read and a subprocess spawn to keep serving the worker.
    fn on_ready(&self, handle: &Arc<WorkerHandle>) {
        let Ok(tokio_runtime) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(worker_fp = %handle.worker_fp,
                "deploy: catchup_attach_failed; no runtime to run it on");
            return;
        };
        let runtime = self.runtime.clone();
        let database = self.database.clone();
        let worker_fp = handle.worker_fp.as_str().to_owned();
        tokio_runtime.spawn(async move {
            let env = ProcessEnv::new();
            let coord_git_sha = build_identity(&env).build_sha;
            let journal = Arc::clone(runtime.journal());
            let deploy_starter =
                move |host: &str, release_sha: &str| start_deploy(&journal, host, Some(release_sha));
            let options = CatchUpDeployOptions {
                deploy_starter: &deploy_starter,
                now_ms: None,
                coord_git_sha: &coord_git_sha,
                env: &env,
            };
            runtime
                .start_catch_up_deploy_on_attach(&database, &worker_fp, &options)
                .await;
        });
    }
}
