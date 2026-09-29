//! The connected keeper every consumer shares, and the only three ways in or
//! out of it. Owned by `runtime`, read by `keeper_pool` and by the open-session
//! set; the admission decision that decides *whether* there is a keeper is
//! `runtime::keeper_boot`'s.

use std::sync::{Arc, Mutex, PoisonError};

use roost_keeper::client::KeeperClient;

/// A connected keeper, shared by everything that talks to it.
///
/// The client's calls take `&self` and it owns a reader thread, so it is shared
/// behind a mutex rather than duplicated. Debug is hand-written: the client
/// holds a socket, and a socket does not belong in a log line.
#[derive(Clone)]
pub struct KeeperHandle(Arc<Mutex<KeeperClient>>);

impl KeeperHandle {
    /// Wrap a client the caller connected itself.
    ///
    /// [`crate::runtime::keeper_boot::ensure_keeper`] is not the only thing that
    /// can produce a connection: a test harness that starts a real keeper, and
    /// any future endpoint source, arrive holding a `KeeperClient` they must not
    /// wrap twice. The shared handle is the type every consumer takes, so the
    /// way IN is as public as the way through it.
    pub fn new(client: KeeperClient) -> Self {
        Self(Arc::new(Mutex::new(client)))
    }

    /// Take the lock and borrow the client.
    ///
    /// The lock is held for the call and no longer, and every client call is a
    /// bounded wait, so a caller must not hold it across anything else. That is
    /// the whole contract, which is why it is one method rather than a public
    /// field.
    pub fn with<T>(&self, use_client: impl FnOnce(&KeeperClient) -> T) -> T {
        match self.0.lock() {
            Ok(client) => use_client(&client),
            // A poisoned lock means a previous holder panicked. The connection
            // is still usable, and refusing here would turn one panicking task
            // into a worker that can never talk to its keeper again.
            Err(poisoned) => use_client(&poisoned.into_inner()),
        }
    }

    /// Drive a different connection from now on (v2 `pool.ensure()` after a
    /// keeper death); the old client is dropped, which stops its reader.
    pub fn replace(&self, client: KeeperClient) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = client;
    }

    /// The client itself, when this is the only handle to it.
    pub fn into_client(self) -> Option<KeeperClient> {
        Arc::try_unwrap(self.0)
            .ok()
            .map(|client| client.into_inner().unwrap_or_else(PoisonError::into_inner))
    }
}

impl std::fmt::Debug for KeeperHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("KeeperHandle")
    }
}
