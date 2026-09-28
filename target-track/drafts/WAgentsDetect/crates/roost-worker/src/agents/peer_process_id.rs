//! Reads the kernel-attested process id of one accepted local report socket.
//! Ports v2 `apps/worker/src/agents/peer-process-id.ts`: the native query is
//! tokio's peer credentials (`SO_PEERCRED` on Linux, `LOCAL_PEERPID` on macOS)
//! instead of Bun FFI, and the reader stays fail-closed. `agents::report_server`
//! consumes only the reader; tests inject the query. The win32 named-pipe
//! query is not ported (Windows is paused).

use std::sync::Arc;

use tokio::net::UnixStream;

/// v2 `NativePeerProcessIdQuery`: the platform's answer for one socket.
/// `Err` is v2's throw; `Ok(None)` is its `null`.
pub trait NativePeerProcessIdQuery: Send + Sync {
    fn read(&self, socket: &UnixStream) -> Result<Option<i64>, String>;

    /// Release whatever the query holds open. The kernel query holds nothing.
    fn close(&self) {}
}

/// The kernel's own peer credentials for a connected Unix socket.
#[derive(Debug, Clone, Copy, Default)]
pub struct KernelPeerCredentials;

impl NativePeerProcessIdQuery for KernelPeerCredentials {
    fn read(&self, socket: &UnixStream) -> Result<Option<i64>, String> {
        let credentials = socket.peer_cred().map_err(|error| error.to_string())?;
        Ok(credentials.pid().map(i64::from))
    }
}

/// v2 `LocalPeerProcessIdReader`. v2's `available` flag reported a failed
/// FFI `dlopen`; the kernel query here has nothing to open, so a reader always
/// has one.
#[derive(Clone)]
pub struct LocalPeerProcessIdReader {
    query: Arc<dyn NativePeerProcessIdQuery>,
}

impl std::fmt::Debug for LocalPeerProcessIdReader {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalPeerProcessIdReader")
            .finish_non_exhaustive()
    }
}

impl LocalPeerProcessIdReader {
    /// The platform reader (v2 `createLocalPeerProcessIdReader()`).
    pub fn native() -> Self {
        Self::with_query(Arc::new(KernelPeerCredentials))
    }

    /// A reader over an injected query (v2 `options.nativeQuery`).
    pub fn with_query(query: Arc<dyn NativePeerProcessIdQuery>) -> Self {
        Self { query }
    }

    /// The peer's pid, or `None` — never a guess. A query failure, a missing
    /// answer and a non-positive or out-of-range pid all fail closed, because
    /// the pid is what authenticates the reporter.
    pub fn read(&self, socket: &UnixStream) -> Option<u32> {
        match self.query.read(socket) {
            Ok(Some(process_id)) if process_id > 0 => u32::try_from(process_id).ok(),
            Ok(_) => None,
            Err(error) => {
                tracing::debug!(%error, "the peer process id query failed; the socket is unauthenticated");
                None
            }
        }
    }

    pub fn close(&self) {
        self.query.close();
    }
}
