//! A keeper `Server` bound in the test process and served on a thread, for the
//! socket tests that need the server's own `ConnectionEnd` or its pid. It binds
//! on the caller's thread, so the socket is connectable the moment `serve`
//! returns, and reports each connection's end through a channel.
// NO `allow(clippy::unwrap_used)` here: see `mod.rs`.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread::JoinHandle;

use roost_keeper::capability::KeeperCapability;
use roost_keeper::server::{ConnectionEnd, Endpoint, Server, UNAUTHENTICATED_TIMEOUT};

use super::daemon::{Client, DEADLINE, TempDir};

/// A server serving a fixed number of connections, one after another.
pub struct InProcessKeeper {
    socket: PathBuf,
    capability: KeeperCapability,
    ends: Receiver<ConnectionEnd>,
    serving: JoinHandle<()>,
}

impl InProcessKeeper {
    /// Bind `temp`'s socket, demanding `temp`'s capability, and serve the next
    /// `connections` connections.
    pub fn serve(temp: &TempDir, connections: usize) -> Self {
        let socket = temp.socket();
        let capability = temp.capability();
        let endpoint = Endpoint::new(socket.clone()).expect("a usable endpoint");
        let mut server = Server::bind(endpoint, capability.clone()).expect("the keeper binds");
        let (report, ends) = mpsc::channel();
        let serving = std::thread::spawn(move || {
            for _ in 0..connections {
                let Some(stream) = server.accept_one() else {
                    return;
                };
                if report.send(server.serve_one(stream)).is_err() {
                    return;
                }
            }
        });
        Self {
            socket,
            capability,
            ends,
            serving,
        }
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    pub fn capability(&self) -> &KeeperCapability {
        &self.capability
    }

    /// An authenticated connection.
    pub fn connect(&self) -> Client {
        Client::connect(&self.socket, &self.capability)
    }

    /// A connection that has sent nothing.
    pub fn connect_unauthenticated(&self) -> Client {
        Client::connect_unauthenticated(&self.socket)
    }

    /// Why the next served connection ended. Bounded past the authentication
    /// timeout, which is the longest a refused connection is held.
    pub fn next_end(&self) -> ConnectionEnd {
        self.ends
            .recv_timeout(UNAUTHENTICATED_TIMEOUT + DEADLINE)
            .expect("the served connection ends before the deadline")
    }

    /// Wait for the serving thread to finish its connections.
    pub fn finish(self) {
        let _ = self.serving.join();
    }
}
