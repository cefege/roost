//! The keeper's socket server: the listener, the per-connection read loop, and
//! the output tick. Everything protocol-shaped lives in [`crate::keeper`], so
//! this file is transport and nothing else.
//!
//! A keeper socket is a remote shell to every PTY the machine has open, so the
//! endpoint is secured before the first connection is accepted rather than
//! after: a listener that exists for a moment with default permissions is a
//! window, and a window on a PTY control socket is a shell.

use crate::keeper::Keeper;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

mod connection;

/// How often the server drains PTY output, AND the longest a single read waits
/// before the loop comes back round to do it.
///
/// ONE constant, because they are the same number. They used to be two:
/// `READ_POLL` at 100 ms and `OUTPUT_TICK` at 16 ms, and the loop blocked in
/// `read()` for the first and THEN slept the second — so a program writing
/// continuously had its output drained once per 116 ms, not once per 16 ms,
/// while the comment above it claimed the tick was "what makes the terminal
/// live". Measured against a real keeper over a real socket, a shell printing
/// 40 lines ten milliseconds apart arrived four lines per 120 ms.
///
/// The two were coupled by accident: `READ_POLL` existed so a stop flag is
/// re-checked promptly, and 16 ms checks it more promptly than 100 ms did.
/// Reading for the tick also removes the sleep, because the timeout is already
/// the pacing.
///
/// It is not only latency. A cell frame is built per PTY chunk, so the chunk
/// rate is the frame rate, and `DRAIN_LIMIT_BYTES` is 16 KiB per tick: at
/// 116 ms a four-megabyte flood took half a minute to land, during which the
/// pane could time out. `crates/roost-worker/src/keeper_pool/dispatch.rs`
/// documented "the keeper's own output tick is 16ms" — false until now.
const OUTPUT_TICK: std::time::Duration = std::time::Duration::from_millis(16);

/// The largest single read. A worker sending a burst of frames is bounded by
/// this, so one client cannot make the keeper allocate without limit.
const READ_BUFFER_BYTES: usize = 64 * 1024;

/// The most output bytes drained from one channel per tick. Bounded so one
/// chatty program cannot starve the others or blow past the frame maximum.
const DRAIN_LIMIT_BYTES: usize = 16 * 1024;

/// Why the keeper could not start.
#[derive(Debug, thiserror::Error)]
pub enum ListenError {
    #[error("the socket path {0} is not inside a directory the keeper may use")]
    UnsafePath(PathBuf),
    #[error("the path {0} exists and is not a socket")]
    NotASocket(PathBuf),
    #[error("a keeper is already listening on {0}")]
    AlreadyRunning(PathBuf),
    #[error("the socket at {path} could not be prepared: {reason}")]
    Prepare { path: PathBuf, reason: String },
    #[error("the socket at {0} could not be bound: {1}")]
    Bind(PathBuf, String),
    #[error("the socket at {path} could not be secured: {reason}")]
    Secure { path: PathBuf, reason: String },
}

/// The endpoint a keeper listens on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    path: PathBuf,
}

impl Endpoint {
    /// A path the keeper will listen on.
    ///
    /// The socket is created by the keeper, so the only decision here is
    /// whether the path is one the keeper should be writing to at all. A path
    /// under a world-writable directory is refused: the socket file's own
    /// permissions do not help if the directory entry can be replaced.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, ListenError> {
        let path = path.into();
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty());
        if let Some(parent) = parent {
            let metadata = std::fs::metadata(parent).map_err(|err| ListenError::Prepare {
                path: path.clone(),
                reason: err.to_string(),
            })?;
            if !metadata.is_dir() {
                return Err(ListenError::UnsafePath(path));
            }
        }
        // Something that is not a socket is never a stale keeper, and removing
        // it would destroy whatever it actually is.
        if let Ok(metadata) = std::fs::symlink_metadata(&path)
            && !metadata.file_type().is_socket()
        {
            return Err(ListenError::NotASocket(path));
        }
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Bind the listener and secure it BEFORE returning.
    ///
    /// The order matters. `bind` creates the socket file with the process
    /// umask's permissions; if we returned first and secured after, anyone who
    /// connected in between would have reached every PTY on the machine.
    pub fn bind(&self) -> Result<UnixListener, ListenError> {
        self.remove_stale_socket()?;
        let listener = UnixListener::bind(&self.path).map_err(|err| {
            if err.kind() == std::io::ErrorKind::AddrInUse {
                ListenError::AlreadyRunning(self.path.clone())
            } else {
                ListenError::Bind(self.path.clone(), err.to_string())
            }
        })?;
        // 0600: the owner may connect, nobody else may. There is no group case
        // to consider — a keeper socket is per-user by construction.
        if let Err(err) =
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))
        {
            return Err(ListenError::Secure {
                path: self.path.clone(),
                reason: err.to_string(),
            });
        }
        Ok(listener)
    }

    /// Remove a socket file left behind by a keeper that died.
    ///
    /// `connect` is how the answer is known rather than guessed: a live keeper
    /// accepts, so a refused connection means the file is stale and a keeper
    /// that refuses connections is not a keeper.
    fn remove_stale_socket(&self) -> Result<(), ListenError> {
        if std::fs::symlink_metadata(&self.path).is_err() {
            return Ok(());
        }
        if UnixStream::connect(&self.path).is_ok() {
            return Err(ListenError::AlreadyRunning(self.path.clone()));
        }
        std::fs::remove_file(&self.path).map_err(|err| ListenError::Prepare {
            path: self.path.clone(),
            reason: err.to_string(),
        })
    }
}

/// How the keeper should stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shutdown {
    /// Stop and keep every PTY alive — the client will find them again.
    StopPreserving,
    /// Stop only if nothing is live. A refusal is reported, not acted on.
    StopIfEmpty,
}

/// Why a connection stopped being served.
///
/// The daemon's exit decision reads this, which is why it is a value and not a
/// bare `return`: "the worker asked me to stop" and "the worker went away" are
/// the same event from the loop's point of view and completely different from
/// the process's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionEnd {
    /// The worker sent `Shutdown`, and every PTY is still live.
    ShutdownRequested,
    /// The worker sent `ShutdownIfEmpty` and the keeper was empty.
    ShutdownIfEmptyAccepted,
    /// The worker sent `ShutdownIfEmpty` while a channel was live. The keeper
    /// stays up, which is the entire reason this frame is separate.
    ShutdownIfEmptyRefused,
    /// The worker sent `Shutdown`, and the decision is the daemon's.
    ShutdownRequestedWithChannels,
    /// The socket closed. The keeper stays up so a reconnecting worker finds
    /// its PTYs — a worker restart must not cost a terminal.
    ClientDisconnected,
    /// The stream violated the protocol. There is no resynchronisation point,
    /// so the connection ends and the keeper stays up.
    ProtocolViolation,
    /// The keeper read something it cannot put in a frame, so this connection
    /// stops being served and the keeper stays up. The drain limit is the
    /// keeper's own constant, three orders of magnitude under the frame bound,
    /// so this is a keeper fault rather than anything the worker did.
    UnframeablePayload,
    /// A write to the worker failed, which is the same thing from here.
    WorkerUnreachable,
}

/// A running keeper, once it is listening.
pub struct Server {
    endpoint: Endpoint,
    listener: UnixListener,
    keeper: Keeper,
}

// The listener is left out: it is the endpoint that identifies this server in
// a log, and a second rendering of the same socket is noise.
impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("endpoint", &self.endpoint)
            .field("channels", &self.keeper.channel_count())
            .finish_non_exhaustive()
    }
}

impl Server {
    pub fn new(endpoint: Endpoint, listener: UnixListener) -> Self {
        Self {
            endpoint,
            listener,
            keeper: Keeper::new(),
        }
    }

    /// Bind an endpoint and start serving it.
    pub fn bind(endpoint: Endpoint) -> Result<Self, ListenError> {
        let listener = endpoint.bind()?;
        Ok(Self::new(endpoint, listener))
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// The keeper behind the socket, for the daemon's exit decision.
    pub fn keeper(&self) -> &Keeper {
        &self.keeper
    }

    /// The keeper behind the socket, mutably, for a caller driving the
    /// dispatcher itself rather than the connection loop.
    pub fn keeper_mut(&mut self) -> &mut Keeper {
        &mut self.keeper
    }

    /// Serve until the listener is closed.
    ///
    /// ONE CONNECTION AT A TIME, deliberately. The keeper's job is to own PTYs,
    /// and a second worker attaching to the same PTYs is not a mode this
    /// protocol has — two workers interleaving writes into one shell is worse
    /// than one waiting.
    ///
    /// That also makes the concurrent-connection count 1, which is the cap. An
    /// earlier draft declared a separate cap constant; it was fiction, because
    /// nothing counted connections and the loop never accepted a second. A
    /// worker that finds the keeper busy waits in the listen backlog, which is
    /// the same protection expressed honestly.
    pub fn serve(&mut self) -> Result<(), ListenError> {
        loop {
            let (stream, _) = match self.listener.accept() {
                Ok(pair) => pair,
                // A client that vanished between the SYN and the accept is
                // ordinary, not a reason to stop.
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => continue,
            };
            self.serve_one(stream);
        }
    }

    /// Accept the next connection, or `None` if one is not waiting.
    ///
    /// Separate from [`Server::serve`] so a test can drive `serve_one` itself
    /// instead of racing the daemon's accept loop.
    pub fn accept_one(&mut self) -> Option<UnixStream> {
        self.listener.accept().ok().map(|(stream, _)| stream)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Leaving the socket file behind makes the next keeper see a stale
        // endpoint, and a stale endpoint that looks live is worse than none.
        let _ = std::fs::remove_file(&self.endpoint.path);
    }
}

/// Whether a shutdown request is one the keeper should honour.
pub fn should_stop(keeper: &Keeper, shutdown: Shutdown) -> bool {
    match shutdown {
        Shutdown::StopPreserving => true,
        Shutdown::StopIfEmpty => keeper.is_empty(),
    }
}
