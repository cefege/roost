//! How a keeper connection is established: connect, retry, and complete the
//! `Hello` handshake. Owned by the worker, through [`crate::client`].
//!
//! This module also owns the WAIT POLICY the retries are expressed in, because a
//! timeout that is not next to the loop that honours it is a number nobody
//! reasons about when it is wrong.
//!
//! Split out because retrying is a policy with its own reasoning, and burying
//! it inside the client's request API makes it invisible. Both halves of the
//! wait look identical from a single attempt: a keeper that is not listening
//! and a keeper that is busy serving another worker.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::client::KeeperClient;
use crate::client_error::ClientError;
use crate::client_io::{Shared, read_frames};

/// Connect to a keeper and complete the `Hello` handshake.
///
/// Retries BOTH the connection and the handshake until
/// `CONNECT_RETRY_TIMEOUT`. A worker frequently starts before the keeper it
/// is meant to own, and a keeper serving another worker accepts the socket
/// and then waits — neither is an error, and both look identical to a
/// single attempt.
pub fn connect(path: impl Into<PathBuf>) -> Result<KeeperClient, ClientError> {
    let path = path.into();
    let deadline = Instant::now() + CONNECT_RETRY_TIMEOUT;
    loop {
        let last = match connect_once(&path) {
            Ok(client) => return Ok(client),
            Err(err) => err,
        };
        if Instant::now() >= deadline {
            return Err(last);
        }
        std::thread::sleep(DEADLINE_TICK);
    }
}

fn connect_once(path: &Path) -> Result<KeeperClient, ClientError> {
    let stream =
        UnixStream::connect(path).map_err(|_| ClientError::NotListening(path.to_path_buf()))?;
    let read_half = stream
        .try_clone()
        .map_err(|err| ClientError::Io(err.to_string()))?;
    stream
        .set_read_timeout(Some(DEADLINE_TICK))
        .map_err(|err| ClientError::Io(err.to_string()))?;

    let (events_tx, events_rx) = std::sync::mpsc::channel();
    let shared = Arc::new(Mutex::new(Shared::default()));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let client = KeeperClient::establish(
        path.to_path_buf(),
        stream,
        Arc::clone(&shared),
        events_rx,
        Arc::clone(&stop),
        std::thread::Builder::new()
            .name("roost-keeper-client".into())
            .spawn({
                let shared = Arc::clone(&shared);
                move || read_frames(read_half, shared, events_tx, Arc::clone(&stop))
            })
            .map_err(|err| ClientError::Io(err.to_string()))?,
    );

    client.hello()?;
    Ok(client)
}

/// How long a healthy keeper has to acknowledge a spawn.
///
/// A healthy keeper acks in well under 100ms. The slack is deliberate: a
/// loaded machine is not a wedged keeper, and killing a keeper that was about
/// to answer costs every live PTY on it.
pub const SPAWN_ACK_TIMEOUT: Duration = Duration::from_secs(8);

/// How long a reconnect is retried before the pool gives up on this keeper.
pub const CONNECT_RETRY_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a handshake may take before the connection is abandoned.
///
/// Generous, because a busy keeper — one still serving another worker — accepts
/// the socket and then says nothing until it finishes. That is not a dead
/// keeper, and treating it as one would fail a worker that was doing nothing
/// wrong.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// How often the client re-checks its own state while idle.
///
/// Short enough that a timeout is reported close to when it happened, long
/// enough that the check is not the dominant cost of an idle worker.
pub const DEADLINE_TICK: Duration = Duration::from_millis(50);
