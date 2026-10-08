//! Reads the kernel-attested process id of one accepted local report
//! connection. Ports v2 `apps/worker/src/agents/peer-process-id.ts`: the native
//! query is tokio's peer credentials on a Unix socket (`SO_PEERCRED` on Linux,
//! `LOCAL_PEERPID` on macOS) and `GetNamedPipeClientProcessId` on a Windows
//! named pipe, instead of Bun FFI, and the reader stays fail-closed.
//! `agents::report_server` consumes only the reader; tests inject the query.

use std::sync::Arc;

/// One accepted report connection: a Unix stream, or a connected named-pipe
/// server instance on Windows.
#[cfg(unix)]
pub type ReportStream = tokio::net::UnixStream;
/// One accepted report connection: a Unix stream, or a connected named-pipe
/// server instance on Windows.
#[cfg(windows)]
pub type ReportStream = tokio::net::windows::named_pipe::NamedPipeServer;

/// v2 `NativePeerProcessIdQuery`: the platform's answer for one socket.
/// `Err` is v2's throw; `Ok(None)` is its `null`.
pub trait NativePeerProcessIdQuery: Send + Sync {
    fn read(&self, socket: &ReportStream) -> Result<Option<i64>, String>;

    /// Release whatever the query holds open. The kernel query holds nothing.
    fn close(&self) {}
}

/// The kernel's own peer credentials for a connected report stream.
#[derive(Debug, Clone, Copy, Default)]
pub struct KernelPeerCredentials;

impl NativePeerProcessIdQuery for KernelPeerCredentials {
    #[cfg(unix)]
    fn read(&self, socket: &ReportStream) -> Result<Option<i64>, String> {
        let credentials = socket.peer_cred().map_err(|error| error.to_string())?;
        Ok(credentials.pid().map(i64::from))
    }

    #[cfg(windows)]
    fn read(&self, socket: &ReportStream) -> Result<Option<i64>, String> {
        roost_keeper::win32_ffi::named_pipe_client_process_id(socket)
            .map(|pid| Some(i64::from(pid)))
            .map_err(|error| error.to_string())
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
    pub fn read(&self, socket: &ReportStream) -> Option<u32> {
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
