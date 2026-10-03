//! Administrative keeper shutdown: the identity-fenced empty shutdown every
//! automatic replacement uses, the unconditional one reserved for an
//! operator-authorized destruction, and the wait that proves the process left.
//! Ports the shutdown half of v2 `apps/worker/src/keeper/keeper-probe.ts`
//! (`shutdownEmptyKeeperAuthenticated`, `shutdownKeeperAuthenticated`,
//! `waitForKeeperExit`). Called by `keeper_pool::update_admission` through
//! `keeper_pool::update_host`, and by `runtime::keeper_boot` for a survivor.
//!
//! The `_on` forms act on a connection the caller already holds; the endpoint
//! forms open one, which the keeper only serves once no other is open.

use std::path::Path;
use std::pin::Pin;
use std::time::{Duration, Instant};

use roost_keeper::client::{KeeperClient, KeeperEndpoint};

use super::runtime_proof::read_runtime_probe;

/// A completed shutdown still reaps every channel and unlinks the endpoint
/// before the process exits, so exit confirmation must outlast that work: a
/// slow exit is not a failed one.
pub const KEEPER_EXIT_CONFIRM_TIMEOUT: Duration = Duration::from_millis(30_000);
pub const KEEPER_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// One reachability attempt while waiting for the exit.
pub const KEEPER_EXIT_PROBE_TIMEOUT: Duration = Duration::from_millis(200);

/// A future an exit watch or update host hands back, borrowing it.
pub type HostFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The identity an empty shutdown must still find: the same process, the same
/// incarnation, and the empty binding set the caller proved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmptyKeeperShutdownExpectation {
    pub keeper_pid: u32,
    pub process_epoch: String,
    pub binding_digest: String,
}

/// What an exit wait polls: reachability, and a clock it may sleep on.
pub trait ExitWatch: Send + Sync {
    /// Whether the endpoint still accepts a transport connection.
    fn reachable(&self) -> HostFuture<'_, bool>;
    fn sleep(&self, duration: Duration) -> HostFuture<'_, ()>;
    /// Monotonic time since an origin the watch chose.
    fn elapsed(&self) -> Duration;
}

/// Poll until the endpoint refuses connections, for at most
/// [`KEEPER_EXIT_CONFIRM_TIMEOUT`]. A refused connection is the only proof the
/// keeper process is gone.
pub async fn wait_for_exit(watch: &dyn ExitWatch) -> bool {
    let deadline = watch.elapsed() + KEEPER_EXIT_CONFIRM_TIMEOUT;
    loop {
        if !watch.reachable().await {
            return true;
        }
        watch.sleep(KEEPER_EXIT_POLL_INTERVAL).await;
        if watch.elapsed() >= deadline {
            tracing::warn!("the keeper endpoint still answered after the exit-confirmation budget");
            return false;
        }
    }
}

/// [`wait_for_exit`] against a socket path, on the real clock.
pub async fn wait_for_keeper_exit(socket: &Path) -> bool {
    wait_for_exit(&SocketExitWatch::new(socket)).await
}

/// Whether anything accepts a connection on the keeper socket within `timeout`.
pub async fn endpoint_reachable(socket: &Path, timeout: Duration) -> bool {
    matches!(
        tokio::time::timeout(timeout, tokio::net::UnixStream::connect(socket)).await,
        Ok(Ok(_))
    )
}

/// Identity-fenced shutdown on a held connection: a fresh `Hello` must still
/// name the expected pid, epoch and empty binding digest, and the keeper checks
/// emptiness again atomically when it answers `ShutdownIfEmpty`.
pub fn shutdown_empty_on(client: &KeeperClient, expected: &EmptyKeeperShutdownExpectation) -> bool {
    let proof = match read_runtime_probe(client).map(|probe| probe.proof()) {
        Ok(Ok(proof)) => proof,
        Ok(Err(refusal)) => {
            tracing::warn!(%refusal, "an empty keeper shutdown found no provable keeper");
            return false;
        }
        Err(error) => {
            tracing::warn!(%error, "an empty keeper shutdown could not re-read the keeper");
            return false;
        }
    };
    if proof.keeper_pid != expected.keeper_pid
        || proof.process_epoch != expected.process_epoch
        || proof.binding_digest() != expected.binding_digest
        || !proof.bindings.is_empty()
        || !proof.spawning_channels.is_empty()
    {
        tracing::warn!(
            keeper_pid = proof.keeper_pid,
            expected_pid = expected.keeper_pid,
            channels = proof.bindings.len(),
            "an empty keeper shutdown was refused: the keeper is not the one admitted"
        );
        return false;
    }
    match client.shutdown_if_empty() {
        Ok(accepted) => {
            tracing::info!(
                accepted,
                keeper_pid = proof.keeper_pid,
                "the keeper answered an empty shutdown"
            );
            accepted
        }
        Err(error) => {
            tracing::warn!(%error, "the keeper did not answer an empty shutdown");
            false
        }
    }
}

/// Unconditional shutdown on a held connection: every PTY the keeper owns ends.
pub fn shutdown_forced_on(client: &KeeperClient) -> bool {
    match client.shutdown() {
        Ok(()) => {
            tracing::warn!("the keeper acknowledged an unconditional shutdown");
            true
        }
        Err(error) => {
            tracing::warn!(%error, "the keeper did not acknowledge an unconditional shutdown");
            false
        }
    }
}

/// [`shutdown_empty_on`] over a fresh connection to `endpoint`.
pub async fn shutdown_empty_keeper_authenticated(
    endpoint: &KeeperEndpoint,
    expected: &EmptyKeeperShutdownExpectation,
) -> bool {
    let expected = expected.clone();
    on_fresh_connection(endpoint, move |client| shutdown_empty_on(client, &expected)).await
}

/// [`shutdown_forced_on`] over a fresh connection to `endpoint`.
pub async fn shutdown_keeper_authenticated(endpoint: &KeeperEndpoint) -> bool {
    on_fresh_connection(endpoint, shutdown_forced_on).await
}

/// Connect, authenticate, act, and drop the connection. `false` when nothing
/// answered: the keeper cannot be shut down by a connection it never accepted.
async fn on_fresh_connection(
    endpoint: &KeeperEndpoint,
    act: impl FnOnce(&KeeperClient) -> bool + Send + 'static,
) -> bool {
    if !endpoint_reachable(&endpoint.socket, KEEPER_EXIT_PROBE_TIMEOUT).await {
        tracing::info!(socket = %endpoint.socket.display(), "no keeper accepted a shutdown connection");
        return false;
    }
    let endpoint = endpoint.clone();
    let acted = tokio::task::spawn_blocking(move || {
        roost_keeper::client::connect(&endpoint).map(|client| act(&client))
    })
    .await;
    match acted {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(error)) => {
            tracing::warn!(%error, "a keeper shutdown connection did not authenticate");
            false
        }
        Err(error) => {
            tracing::error!(%error, "a keeper shutdown task did not finish");
            false
        }
    }
}

/// The real socket and the real clock.
#[derive(Debug)]
pub struct SocketExitWatch {
    socket: std::path::PathBuf,
    origin: Instant,
}

impl SocketExitWatch {
    pub fn new(socket: &Path) -> Self {
        Self {
            socket: socket.to_path_buf(),
            origin: Instant::now(),
        }
    }
}

impl ExitWatch for SocketExitWatch {
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
