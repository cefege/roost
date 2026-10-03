//! A real keeper daemon behind a real Unix socket, for the client tests.
//! Every answer a client test asserts on comes from the `roost-keeper` binary,
//! because the daemon is the only reference for what a frame means — a stub
//! would prove that the stub agrees with the client, which is the defect this
//! crate has already shipped twice.
//!
//! Also here, the two degenerate keepers a client must survive: one that takes
//! the connection and never answers, and one that is gone.
// NO `allow(clippy::unwrap_used)` here, deliberately, and for the same reason
// as in `mod.rs`: the allow belongs to the compilation unit, and this file's
// only consumer is `support/mod.rs`, whose own consumers declare it at their
// roots. A declaration here would be a second one to keep in step.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use roost_keeper::codec::{FrameDecoder, MuxFrame, StreamEvent};

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

    pub fn pid(&self) -> u32 {
        self.child.id()
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

/// A worker's side of a daemon connection: frames out, frames back, every read
/// bounded by `DEADLINE`.
pub struct Client {
    stream: UnixStream,
    decoder: FrameDecoder,
}

impl Client {
    pub fn connect(path: &Path) -> Self {
        let stream = UnixStream::connect(path).expect("the keeper is listening");
        Self {
            stream,
            decoder: FrameDecoder::new(),
        }
    }

    pub fn send(&mut self, frame: &MuxFrame) {
        self.stream
            .write_all(&frame.encode())
            .expect("a write to a live socket");
    }

    /// Read frames until `predicate` holds over everything seen, failing at the
    /// deadline with `what` and the frame types that did arrive.
    pub fn read_until(
        &mut self,
        what: &str,
        mut predicate: impl FnMut(&[MuxFrame]) -> bool,
    ) -> Vec<MuxFrame> {
        let start = Instant::now();
        let mut seen: Vec<MuxFrame> = Vec::new();
        let mut chunk = vec![0u8; 4096];
        while start.elapsed() < DEADLINE {
            if let Ok(read) = self.stream.read(&mut chunk) {
                if read == 0 {
                    break;
                }
                for event in self.decoder.push(&chunk[..read]) {
                    if let StreamEvent::Frame {
                        frame_type: Some(frame_type),
                        channel_id,
                        payload,
                        ..
                    } = event
                    {
                        seen.push(MuxFrame {
                            frame_type,
                            channel_id,
                            payload,
                        });
                    }
                }
            }
            if predicate(&seen) {
                return seen;
            }
        }
        panic!(
            "never saw {what} within {DEADLINE:?}; saw {:?}",
            seen.iter().map(|f| f.frame_type).collect::<Vec<_>>()
        );
    }

    /// Whether the daemon closed this connection within the deadline: a read
    /// returns end-of-file, or fails because the peer is gone.
    pub fn closed_by_peer(&mut self) -> bool {
        let _ = self.stream.set_read_timeout(Some(DEADLINE));
        let mut chunk = vec![0u8; 4096];
        let start = Instant::now();
        while start.elapsed() < DEADLINE {
            match self.stream.read(&mut chunk) {
                Ok(0) | Err(_) => return true,
                Ok(_) => continue,
            }
        }
        false
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
