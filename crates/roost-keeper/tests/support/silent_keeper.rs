//! The degenerate keeper a client must survive: a socket that accepts
//! connections, holds them open, and never says anything. Used by the client
//! tests that prove every keeper wait is bounded.
// NO `allow(clippy::unwrap_used)` here: see `mod.rs`.

use std::os::unix::net::UnixListener;
use std::path::PathBuf;

use super::daemon::TempDir;

/// The 2026-06-22 incident as a fixture: a degraded keeper that takes the
/// connection and never answers. A client that cannot survive it has no bounded
/// wait, and that is invisible until a keeper wedges in production.
pub struct SilentKeeper {
    socket: PathBuf,
    _listener: UnixListener,
}

impl SilentKeeper {
    pub fn start(temp: &TempDir) -> Self {
        let socket = temp.socket();
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).expect("a listening socket");
        // The accept loop owns a clone so the listener outlives it if the test
        // ends first. Accepted connections are kept, unwritten, until the test
        // process ends: a closed peer would be an answer.
        let accepting = listener.try_clone().expect("a second listener handle");
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for stream in accepting.incoming() {
                let Ok(stream) = stream else { break };
                held.push(stream);
            }
        });
        Self {
            socket,
            _listener: listener,
        }
    }

    pub fn socket(&self) -> &PathBuf {
        &self.socket
    }
}

impl Drop for SilentKeeper {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket);
    }
}
