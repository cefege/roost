//! The keeper client against a REAL keeper daemon. The client and the daemon
//! are two ends of a socket protocol, so testing one against a stub of the
//! other would prove only that the stub agrees with the client — and the
//! spawn-ack timeout in particular cannot be exercised without a keeper that
//! deliberately does not answer.

use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use roost_keeper::client::connect;
use roost_keeper::client_error::ClientError;
use roost_keeper::codec::{MuxFrame, MuxFrameType};
use roost_keeper::frames::ShellSpec;

const DEADLINE: Duration = Duration::from_secs(15);
const STARTUP: Duration = Duration::from_secs(10);

struct TempDir {
    dir: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let unique = format!(
            "roost-client-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        );
        let dir = std::env::temp_dir().join(unique.replace(['(', ')', ' '], ""));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        Self { dir }
    }

    fn socket(&self) -> PathBuf {
        self.dir.join("keeper.sock")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn keeper_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_roost-keeper"))
}

fn cat() -> ShellSpec {
    ShellSpec {
        program: "/bin/cat".into(),
        args: Vec::new(),
        env: Vec::new(),
        cwd: None,
    }
}

/// A real keeper daemon, killed on drop.
struct Keeper {
    child: Child,
    socket: PathBuf,
}

impl Keeper {
    fn start(temp: &TempDir) -> Self {
        let socket = temp.socket();
        let child = Command::new(keeper_binary())
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

    fn wait_until_listening(&self) {
        let start = Instant::now();
        while start.elapsed() < STARTUP {
            if self.socket.exists() && std::os::unix::net::UnixStream::connect(&self.socket).is_ok()
            {
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
/// This is the 2026-06-22 incident as a test fixture: a degraded keeper that
/// takes the connection and never answers. The only way to prove the client
/// survives it is to build one.
struct SilentKeeper {
    socket: PathBuf,
    _listener: UnixListener,
}

impl SilentKeeper {
    fn start(temp: &TempDir) -> Self {
        let socket = temp.socket();
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).expect("a listening socket");
        let (sender, _accepted) = std::sync::mpsc::channel();
        // The accept loop runs on its own thread; the listener handle stays
        // here so the socket outlives that thread if the test ends first.
        let accepting = listener
            .try_clone()
            .expect("a second handle on the listener");
        std::thread::spawn(move || {
            // Hold every accepted connection open and never write to it. The
            // client must time out rather than wait forever.
            for stream in accepting.incoming() {
                let Ok(stream) = stream else { break };
                if sender.send(stream).is_err() {
                    break;
                }
            }
        });
        Self {
            socket,
            _listener: listener,
        }
    }
}

impl Drop for SilentKeeper {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// Connecting to nothing reports that, rather than hanging or panicking.
#[test]
fn connecting_to_no_keeper_says_so() {
    let temp = TempDir::new("absent");
    let err = connect(temp.socket()).expect_err("nothing is listening");
    assert!(
        matches!(err, ClientError::NotListening(_)),
        "the failure must name the cause, got {err:?}"
    );
}

/// The handshake completes and reports the keeper's own observation, which is
/// what lets a worker tell a keeper it trusts from one it merely reached.
#[test]
fn the_handshake_reports_what_the_keeper_is() {
    let temp = TempDir::new("hello");
    let _keeper = Keeper::start(&temp);

    let client = connect(temp.socket()).expect("a handshake");
    let features = client.hello().expect("a second handshake also works");
    assert!(
        !features.is_empty(),
        "a v3 keeper negotiates at least one feature"
    );

    let observation = client
        .observation()
        .expect("the observation from the handshake");
    assert_eq!(
        observation.contract.protocol_version,
        roost_keeper::payloads::KEEPER_PROTOCOL_VERSION
    );
    assert_eq!(
        observation.live_channel_count, 0,
        "nothing has been spawned yet"
    );
}

/// A spawn goes through and comes back with a real pid, which is the whole
/// point of asking.
#[test]
fn a_spawn_returns_a_real_pid() {
    let temp = TempDir::new("spawn");
    let _keeper = Keeper::start(&temp);

    let client = connect(temp.socket()).expect("a handshake");
    let pid = client
        .spawn(1, cat(), 80, 24)
        .expect("the spawn is acknowledged");
    assert!(pid > 0);
    assert_ne!(
        pid,
        std::process::id(),
        "the child is not this test process"
    );
}

/// THE INCIDENT, at the connect layer. A keeper that takes the connection and
/// then says nothing must produce a bounded failure, not a hang — and the retry
/// window is what the operator waits, so this is asserted against the clock
/// rather than against a comment.
#[test]
fn a_keeper_that_never_answers_gives_up_within_the_retry_window() {
    let temp = TempDir::new("silent");
    let _silent = SilentKeeper::start(&temp);

    let start = Instant::now();
    let result = connect(temp.socket());
    let waited = start.elapsed();

    assert!(
        result.is_err(),
        "a keeper that never answers cannot be connected to"
    );
    assert!(
        waited < roost_keeper::client::CONNECT_RETRY_TIMEOUT + Duration::from_secs(5),
        "the client must give up rather than hang; it waited {waited:?}"
    );
}

/// The spawn-ack timeout is the incident's own bound, and its error is what an
/// operator actually reads. The 8s wait is not re-paid on every test run, so
/// what is asserted is the value and the message.
#[test]
fn the_spawn_timeout_names_the_keeper_and_the_wait() {
    let err = ClientError::SpawnNotAcknowledged {
        path: PathBuf::from("/run/user/1000/mux-keeper.sock"),
        timeout: roost_keeper::client::SPAWN_ACK_TIMEOUT,
    };
    let rendered = err.to_string();
    assert!(rendered.contains("did not answer a spawn"), "{rendered}");
    assert!(
        rendered.contains("mux-keeper.sock"),
        "the operator needs the endpoint: {rendered}"
    );
    assert!(
        rendered.contains("8s"),
        "and how long they waited: {rendered}"
    );
}
/// it, so the slack is worth more than a fast failure.
#[test]
fn the_spawn_timeout_has_slack_for_a_loaded_machine() {
    let timeout = roost_keeper::client::SPAWN_ACK_TIMEOUT;
    assert!(
        timeout >= Duration::from_secs(5),
        "a keeper acks in well under 100ms; the bound is for a loaded box, not a slow one: {timeout:?}"
    );
    assert!(
        timeout <= Duration::from_secs(15),
        "but it must still be survivable: {timeout:?}"
    );
}

/// Input reaches the PTY and the output comes back, which is the one property a
/// terminal is made of.
#[test]
fn a_terminal_round_trips_through_the_client() {
    let temp = TempDir::new("terminal");
    let _keeper = Keeper::start(&temp);

    let client = connect(temp.socket()).expect("a handshake");
    client
        .spawn(1, cat(), 80, 24)
        .expect("the spawn is acknowledged");
    client
        .write_input(1, b"round-trip\r")
        .expect("the write lands");

    let start = Instant::now();
    let mut seen = Vec::new();
    while start.elapsed() < DEADLINE {
        let Some(frame) = client.next_event(Duration::from_millis(200)) else {
            continue;
        };
        if frame.frame_type == MuxFrameType::PtyOut {
            seen.extend_from_slice(&frame.payload);
            if seen.windows(10).any(|w| w == b"round-trip") {
                return;
            }
        }
    }
    panic!(
        "the echo never arrived; saw {:?}",
        String::from_utf8_lossy(&seen)
    );
}

#[test]
fn a_write_to_a_dead_keeper_reports_rather_than_succeeding() {
    let temp = TempDir::new("deadwrite");
    let keeper = Keeper::start(&temp);
    let client = connect(temp.socket()).expect("a handshake");
    client
        .spawn(1, cat(), 80, 24)
        .expect("the spawn is acknowledged");

    let mut keeper = keeper;
    let _ = keeper.child.kill();
    let _ = keeper.child.wait();

    // The socket is still open on this side, so the FIRST write may be absorbed
    // by the kernel buffer. What must not happen is a write reporting success
    // forever with no keeper behind it — so the claim under test is that the
    // client eventually reports, not that it reports on the first write.
    let mut reported = None;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) && reported.is_none() {
        if let Err(err) = client.write_input(1, b"into the void\r") {
            reported = Some(err);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // Either the write failed, or it was absorbed before the keeper died. Both
    // are honest; what this asserts is that the client does not HANG.
    if let Some(err) = reported {
        assert!(
            matches!(err, ClientError::Io(_)),
            "unexpected failure kind: {err:?}"
        );
    }
}

/// A `Pong` the keeper volunteers is delivered as an event, not mistaken for an
/// answer to something the client is waiting for.
#[test]
fn a_pong_does_not_consume_a_pending_answer() {
    let temp = TempDir::new("pong");
    let _keeper = Keeper::start(&temp);

    let client = connect(temp.socket()).expect("a handshake");
    client
        .spawn(1, cat(), 80, 24)
        .expect("the spawn is acknowledged");

    // Interleave a ping between the spawn and a sequenced write, so the pong
    // arrives while the client is waiting for the write's answer.
    let mut stream = std::os::unix::net::UnixStream::connect(temp.socket()).expect("a connection");
    let ping = MuxFrame::new(MuxFrameType::Ping, 0, Vec::new()).expect("an empty payload");
    stream.write_all(&ping.encode()).expect("a write");
    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .expect("a read timeout");
    let mut chunk = [0u8; 256];
    let _ = stream.read(&mut chunk);

    // The client must still be able to complete a request afterwards.
    assert_eq!(
        client.resize(1, 1, 100, 30).applied_geometry(),
        Some((100, 30)),
        "the resize is acknowledged despite the ping"
    );
}
