//! A real keeper daemon behind a real Unix socket, for the client tests.
//! Every answer a client test asserts on comes from the `roost-keeper` binary,
//! because the daemon is the only reference for what a frame means — a stub
//! would prove that the stub agrees with the client, which is the defect this
//! crate has already shipped twice.
//!
//! Also here, the worker's end of a connection (`Client`), which authenticates
//! with the capability before it is handed to a test. The degenerate keeper a
//! client must survive lives in `silent_keeper.rs`.
// NO `allow(clippy::unwrap_used)` here, deliberately, and for the same reason
// as in `mod.rs`: the allow belongs to the compilation unit, and this file's
// only consumer is `support/mod.rs`, whose own consumers declare it at their
// roots. A declaration here would be a second one to keep in step.

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use roost_keeper::capability::KeeperCapability;
use roost_keeper::client::KeeperEndpoint;
use roost_keeper::codec::{FrameDecoder, MuxFrame, MuxFrameType, StreamEvent};
use roost_keeper::payloads::KeeperHelloResponse;

use super::hello_frame;

/// How long a daemon has to start listening.
pub const STARTUP: Duration = Duration::from_secs(10);

/// Every wait below fails at a deadline rather than blocking, because a test
/// that hangs is indistinguishable from a keeper that does.
pub const DEADLINE: Duration = Duration::from_secs(10);

/// How long one socket read may block, so every read loop re-checks its
/// deadline instead of waiting on a peer that never writes.
const READ_POLL: Duration = Duration::from_millis(50);

/// A directory this test owns, removed on drop. It plays the worker's data
/// directory: the socket, the pid file and the capability file live in it.
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
        // `/tmp`, not `std::env::temp_dir()`: a Unix socket path must fit
        // `sun_path` (104 bytes on macOS), and macOS's per-user `$TMPDIR`
        // spends about half of that before this directory is named.
        let dir = Path::new("/tmp").join(unique);
        std::fs::create_dir_all(&dir).expect("a temp dir");
        Self { dir }
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }

    pub fn socket(&self) -> PathBuf {
        self.dir.join("keeper.sock")
    }

    pub fn pid_file(&self) -> PathBuf {
        self.dir.join("keeper.pid")
    }

    pub fn capability_file(&self) -> PathBuf {
        self.dir.join("mux-keeper.cap")
    }

    /// The capability in this directory, minted on first use as the worker
    /// does, so a keeper started here and every client of it agree on it.
    pub fn capability(&self) -> KeeperCapability {
        KeeperCapability::load_or_create(&self.capability_file())
            .expect("the capability file is writable")
    }

    /// Everything a library client needs to dial a keeper in this directory.
    pub fn endpoint(&self) -> KeeperEndpoint {
        KeeperEndpoint {
            socket: self.socket(),
            capability: self.capability(),
        }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The real daemon, killed on drop. `has_exited` is how a test observes a
/// shutdown: the daemon's decision to stop is a process exit, and nothing on
/// the wire says so.
pub struct Keeper {
    child: Child,
    socket: PathBuf,
    capability: KeeperCapability,
}

impl Keeper {
    /// A daemon on `temp`'s socket with no pid file, demanding `temp`'s
    /// capability.
    pub fn start(temp: &TempDir) -> Self {
        Self::launch(temp, None)
    }

    /// The same daemon, publishing its pid at `temp.pid_file()`.
    pub fn start_with_pid_file(temp: &TempDir) -> Self {
        Self::launch(temp, Some(&temp.pid_file()))
    }

    fn launch(temp: &TempDir, pid_file: Option<&Path>) -> Self {
        let socket = temp.socket();
        // The keeper never creates the capability, so it must exist before the
        // daemon reads it at startup.
        let capability = temp.capability();
        let mut command = Command::new(keeper_binary());
        command
            .arg("--socket")
            .arg(&socket)
            .arg("--capability-file")
            .arg(temp.capability_file());
        if let Some(pid_file) = pid_file {
            command.arg("--pid-file").arg(pid_file);
        }
        let child = command
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the keeper binary starts");
        let mut keeper = Self {
            child,
            socket,
            capability,
        };
        keeper.wait_until_listening();
        keeper
    }

    pub fn socket(&self) -> &PathBuf {
        &self.socket
    }

    pub fn capability(&self) -> &KeeperCapability {
        &self.capability
    }

    pub fn endpoint(&self) -> KeeperEndpoint {
        KeeperEndpoint {
            socket: self.socket.clone(),
            capability: self.capability.clone(),
        }
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// An authenticated wire-level connection to this daemon.
    pub fn connect(&self) -> Client {
        Client::connect(&self.socket, &self.capability)
    }

    /// Kill the daemon as a crash would, and reap it.
    pub fn kill_and_reap(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    pub fn has_exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    /// The probe is a raw connect dropped at once: the daemon reads end-of-file
    /// before any `Hello` and goes back to accepting.
    fn wait_until_listening(&mut self) {
        let start = Instant::now();
        while start.elapsed() < STARTUP {
            if let Ok(Some(status)) = self.child.try_wait() {
                panic!("the keeper exited before it listened: {status}");
            }
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
        self.kill_and_reap();
    }
}

/// A worker's side of a keeper connection: frames out, frames back, every read
/// bounded by `DEADLINE`.
pub struct Client {
    stream: UnixStream,
    decoder: FrameDecoder,
    /// Frames decoded but not yet handed out: whatever arrived behind the frame
    /// that ended the last `read_until`, in the same read, kept for the next.
    pending: VecDeque<MuxFrame>,
    hello: Option<KeeperHelloResponse>,
}

impl Client {
    /// Connect and authenticate: the `Hello` carrying `capability` is sent and
    /// its `HelloResp` must be the first frame back before this returns.
    pub fn connect(path: &Path, capability: &KeeperCapability) -> Self {
        let mut client = Self::connect_unauthenticated(path);
        client.send(&hello_frame(capability.as_str()));
        let mut seen = client.read_until("the HelloResp", |frames| {
            frames
                .iter()
                .any(|frame| frame.frame_type == MuxFrameType::HelloResp)
        });
        let answer = seen.remove(0);
        assert_eq!(
            answer.frame_type,
            MuxFrameType::HelloResp,
            "nothing reaches a connection before its HelloResp"
        );
        client.hello = Some(answer.parse_json().expect("the HelloResp decodes"));
        client
    }

    /// A connection that has sent nothing yet, for tests about what the keeper
    /// does before a `Hello`.
    pub fn connect_unauthenticated(path: &Path) -> Self {
        let stream = UnixStream::connect(path).expect("the keeper is listening");
        stream
            .set_read_timeout(Some(READ_POLL))
            .expect("a read timeout");
        Self {
            stream,
            decoder: FrameDecoder::new(),
            pending: VecDeque::new(),
            hello: None,
        }
    }

    /// The keeper's answer to this connection's `Hello`.
    pub fn hello_response(&self) -> &KeeperHelloResponse {
        self.hello
            .as_ref()
            .expect("only an authenticated client has a HelloResp")
    }

    pub fn send(&mut self, frame: &MuxFrame) {
        self.stream
            .write_all(&frame.encode())
            .expect("a write to a live socket");
    }

    /// Write bytes that need not be a frame. The result is returned because a
    /// keeper that closes mid-write is what some callers are proving.
    pub fn send_raw(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.stream.write_all(bytes)
    }

    /// Read frames until `predicate` holds over everything seen, failing at the
    /// deadline with `what` and the frame types that did arrive.
    ///
    /// Returns the SHORTEST run of frames that satisfies `predicate`; anything
    /// decoded behind it stays in `pending` for the next call. The keeper
    /// writes a spawn's ack and its child's first output back to back, so a
    /// reader that falls behind takes both in one read, and a wait that
    /// returned both would hand the next wait a stream whose output was gone.
    pub fn read_until(
        &mut self,
        what: &str,
        mut predicate: impl FnMut(&[MuxFrame]) -> bool,
    ) -> Vec<MuxFrame> {
        let start = Instant::now();
        let mut seen: Vec<MuxFrame> = Vec::new();
        let mut chunk = vec![0u8; 4096];
        loop {
            if predicate(&seen) {
                return seen;
            }
            if let Some(frame) = self.pending.pop_front() {
                seen.push(frame);
                continue;
            }
            if start.elapsed() >= DEADLINE {
                break;
            }
            match self.stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => {
                    for event in self.decoder.push(&chunk[..read]) {
                        if let StreamEvent::Frame {
                            frame_type: Some(frame_type),
                            channel_id,
                            payload,
                            ..
                        } = event
                        {
                            self.pending.push_back(MuxFrame {
                                frame_type,
                                channel_id,
                                payload,
                            });
                        }
                    }
                }
                Err(_) => {}
            }
        }
        panic!(
            "never saw {what} within {DEADLINE:?}; saw {:?}",
            seen.iter().map(|f| f.frame_type).collect::<Vec<_>>()
        );
    }

    /// Every byte the keeper sent before it closed this connection, or `None`
    /// when it was still open at the deadline. A read that merely timed out is
    /// not a close; end-of-file or any other failure is.
    pub fn read_until_closed(&mut self) -> Option<Vec<u8>> {
        let mut received = Vec::new();
        let mut chunk = vec![0u8; 4096];
        let start = Instant::now();
        while start.elapsed() < DEADLINE {
            match self.stream.read(&mut chunk) {
                Ok(0) => return Some(received),
                Ok(read) => received.extend_from_slice(&chunk[..read]),
                Err(err) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(_) => return Some(received),
            }
        }
        None
    }

    /// Whether the keeper closed this connection within the deadline.
    pub fn closed_by_peer(&mut self) -> bool {
        self.read_until_closed().is_some()
    }
}

/// The keeper binary cargo built for this test run. `CARGO_BIN_EXE_<name>` is
/// set for integration tests of a crate with binaries, so this is never a
/// guess at where it landed.
pub fn keeper_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_roost-keeper"))
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
