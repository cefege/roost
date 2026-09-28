//! The production keeper I/O behind a keeper update: proof, shutdown and exit
//! watch over this worker's pool connection, falling back to a fresh
//! connection only when the pool has none. Ports the production dependencies
//! v2 `apps/worker/src/keeper/update-admission.ts` defaulted to
//! (`probeKeeperCompatible`, `shutdown*KeeperAuthenticated`, `Bun.sleep`,
//! `Date.now`). Built by `runtime::owners`; driven by `keeper_pool::update_admission`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::keeper_shutdown::{
    EmptyKeeperShutdownExpectation, ExitWatch, HostFuture, KEEPER_EXIT_PROBE_TIMEOUT,
    endpoint_reachable, shutdown_empty_keeper_authenticated, shutdown_empty_on,
    shutdown_forced_on, shutdown_keeper_authenticated,
};
use super::pool::KeeperPool;
use super::runtime_proof::{KeeperRuntimeProbe, probe_endpoint};
use super::update_admission::KeeperUpdateHost;

/// This worker's keeper: its pool connection and the socket it was dialled on.
#[derive(Debug)]
pub struct PoolKeeperHost {
    pool: Arc<KeeperPool>,
    socket: PathBuf,
    origin: Instant,
}

impl PoolKeeperHost {
    pub fn new(pool: Arc<KeeperPool>, socket: &Path) -> Self {
        Self {
            pool,
            socket: socket.to_path_buf(),
            origin: Instant::now(),
        }
    }

    /// Run a shutdown on the pool's connection, or on a fresh one when the pool
    /// has none. A shutdown the keeper accepted ends the pool's connection, so
    /// the pool is told its keeper is gone rather than finding out on a write.
    async fn shut_down(
        &self,
        on_pool: impl FnOnce(&roost_keeper::client::KeeperClient) -> bool + Send + 'static,
        fresh: HostFuture<'_, bool>,
        what: &'static str,
    ) -> bool {
        if !self.pool.is_connected() {
            return fresh.await;
        }
        let pool = Arc::clone(&self.pool);
        let asked = tokio::task::spawn_blocking(move || pool.keeper.with(on_pool)).await;
        let accepted = asked.unwrap_or_else(|error| {
            tracing::error!(%error, "a keeper shutdown task did not finish");
            false
        });
        if accepted {
            self.pool.keeper_lost(format!("the keeper accepted {what}"));
        }
        accepted
    }
}

impl ExitWatch for PoolKeeperHost {
    fn reachable(&self) -> HostFuture<'_, bool> {
        Box::pin(endpoint_reachable(&self.socket, KEEPER_EXIT_PROBE_TIMEOUT))
    }

    fn sleep(&self, duration: Duration) -> HostFuture<'_, ()> {
        Box::pin(tokio::time::sleep(duration))
    }

    fn elapsed(&self) -> Duration {
        self.origin.elapsed()
    }
}

impl KeeperUpdateHost for PoolKeeperHost {
    fn probe(&self) -> HostFuture<'_, KeeperRuntimeProbe> {
        Box::pin(async move {
            match self.pool.probe_runtime().await {
                Ok(probe) => probe,
                Err(error) => {
                    tracing::info!(%error, "the pool cannot prove its keeper; probing the endpoint afresh");
                    probe_endpoint(&self.socket).await
                }
            }
        })
    }

    fn shutdown_empty(&self, expected: EmptyKeeperShutdownExpectation) -> HostFuture<'_, bool> {
        Box::pin(async move {
            let on_pool = expected.clone();
            let fresh = Box::pin(async move {
                shutdown_empty_keeper_authenticated(&self.socket, &expected).await
            });
            self.shut_down(
                move |client| shutdown_empty_on(client, &on_pool),
                fresh,
                "an empty shutdown",
            )
            .await
        })
    }

    fn shutdown_forced(&self) -> HostFuture<'_, bool> {
        Box::pin(async move {
            let fresh = Box::pin(shutdown_keeper_authenticated(&self.socket));
            self.shut_down(shutdown_forced_on, fresh, "an unconditional shutdown")
                .await
        })
    }
}
