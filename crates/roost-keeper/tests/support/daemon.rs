//! A real keeper daemon behind a real Unix socket, for the client tests.
//! Every answer a client test asserts on comes from the `roost-keeper` binary,
//! because the daemon is the only reference for what a frame means — a stub
//! would prove that the stub agrees with the client, which is the defect this
//! crate has already shipped twice.
//!
//! Also here, the two degenerate keepers a client must survive: one that takes
//! the connection and never answers, and one that is gone.

use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// How long a daemon has to start listening.
pub const STARTUP: Duration = Duration::from_secs(10);

/// Every wait below fails at a deadline rather than blocking, because a test
/// that hangs is indistinguishable from a keeper that does.
pub const DEADLINE: Duration = Duration::from_secs(10);

/// A directory this test owns, removed on drop.
pub struct TempDir {
    dir: PathBuf,
}

impl TempDir {
    pub fn new(label: &str) -> Self {
        let unique = format!(
            "roost-daemon-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        )
        .replace(['(', ')', ' '], "");
        let dir = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&dir).expect("a temp dir");
        Self { dir }
    }

    pub fn socket(&self) -> PathBuf {
        self.dir.join("keeper.sock")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The real daemon, killed on drop. `has_exited` is how a test observes a
/// shutdown: the daemon's decision to stop is a process exit
/// (`bin/roost-keeper.rs:125`), and nothing on the wire says so.
pub struct Keeper {
    child: Child,
    socket: PathBuf,
}

impl Keeper {
    pub fn start(temp: &TempDir) -> Self {
        let socket = temp.socket();
        let child = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_roost-keeper")))
            .arg("--socket")
            .arg(&socket)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the keeper binary starts");
        let keeper = Self { child, socket };
        keeper.wait_until_listening();
        keeper
    }

    pub fn socket(&self) -> &PathBuf {
        &self.socket
    }

    pub fn has_exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    fn wait_until_listening(&self) {
        let start = Instant::now();
        while start.elapsed() < STARTUP {
            if self.socket.exists() && UnixStream::connect(&self.socket).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!(
            "the keeper never started listening on {}",
            self.socket.display()
        );
    }
}

impl Drop for Keeper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A socket that accepts a connection and then says nothing at all.
///
/// The 2026-06-22 incident as a fixture. A client that cannot survive it has no
/// bounded wait, and that is invisible until a keeper wedges in production.
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
        // ends first, and every accepted connection is dropped immediately:
        // the client must see a closed peer, never an answer.
        let accepting = listener.try_clone().expect("a second listener handle");
        std::thread::spawn(move || {
            for stream in accepting.incoming() {
                drop(stream);
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

/// Wait for a condition that is about the daemon rather than about a clock,
/// failing at the deadline with `what` rather than hanging.
pub fn wait_until(what: &str, mut predicate: impl FnMut() -> bool) {
    let start = Instant::now();
    while start.elapsed() < DEADLINE {
        if predicate() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("{what} never happened within {DEADLINE:?}");
}
