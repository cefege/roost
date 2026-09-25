//! What the keeper daemon PUBLISHES on the filesystem: the endpoint's
//! permissions, the pid file's permissions, and the pid file's lifetime. Split
//! from the lifecycle tests because a failure in one is a permissions or
//! cleanup regression, and they are worth reading as a group.
//!
//! A keeper socket is a remote shell to every PTY on the machine. A readable
//! pid file tells any local process where to send a signal. Both are
//! filesystem properties, which is why they are tested without a conversation.

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use roost_keeper::codec::{FrameDecoder, MuxFrame, MuxFrameType, StreamEvent};
use roost_keeper::payloads::PtyInRequest;

const DEADLINE: Duration = Duration::from_secs(15);
const STARTUP: Duration = Duration::from_secs(10);

struct TempDir {
    dir: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let unique = format!(
            "roost-keeper-files-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        );
        let dir = std::env::temp_dir().join(unique.replace(['(', ')', ' '], ""));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        Self { dir }
    }

    fn join(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn keeper_binary() -> PathBuf {
    // `CARGO_BIN_EXE_<name>` is the binary cargo just built, not a guess.
    PathBuf::from(env!("CARGO_BIN_EXE_roost-keeper"))
}

struct Keeper {
    child: Child,
    socket: PathBuf,
}

impl Keeper {
    fn start(temp: &TempDir) -> Self {
        let socket = temp.join("keeper.sock");
        let pid_file = temp.join("keeper.pid");
        let child = Command::new(keeper_binary())
            .arg("--socket")
            .arg(&socket)
            .arg("--pid-file")
            .arg(&pid_file)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the keeper binary starts");
        let keeper = Self { child, socket };
        keeper.wait_until_listening();
        keeper
    }

    /// The pid file is written BEFORE the socket is published, so reaching
    /// "listening" also proves the pid file is already there.
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

    fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for Keeper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Read frames until the connection closes, which is what a shutdown produces.
fn wait_for_close(socket: &std::path::Path) {
    let mut stream = UnixStream::connect(socket).expect("the keeper is listening");
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .expect("a read timeout");
    let shutdown = MuxFrame::new(MuxFrameType::Shutdown, 0, Vec::new()).expect("an empty payload");
    stream.write_all(&shutdown.encode()).expect("a write");

    let mut decoder = FrameDecoder::new();
    let mut chunk = vec![0u8; 4096];
    let start = Instant::now();
    let mut saw_ack = false;
    while start.elapsed() < DEADLINE {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                for event in decoder.push(&chunk[..read]) {
                    if let StreamEvent::Frame {
                        frame_type: Some(MuxFrameType::ShutdownAck),
                        ..
                    } = event
                    {
                        saw_ack = true;
                    }
                }
            }
            Err(_) => continue,
        }
    }
    assert!(saw_ack, "the shutdown was never acknowledged");
}

/// The endpoint is owner-only from the moment it exists. A keeper socket is a
/// remote shell to every PTY on the machine, so a world-readable one is a shell
/// for anyone on the box.
#[test]
fn the_daemon_publishes_an_owner_only_endpoint() {
    let temp = TempDir::new("perms");
    let _keeper = Keeper::start(&temp);
    let mode = std::fs::metadata(temp.join("keeper.sock"))
        .expect("the socket")
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o777,
        0o600,
        "the socket is owner-only, not {mode:o}"
    );
}

/// The pid file is owner-only for the same reason, one step removed: a readable
/// pid file tells any local process where to send a signal.
#[test]
fn the_daemon_writes_an_owner_only_pid_file() {
    let temp = TempDir::new("pidperm");
    let keeper = Keeper::start(&temp);
    let _ = &keeper;
    let mode = std::fs::metadata(temp.join("keeper.pid"))
        .expect("the pid file")
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o777,
        0o600,
        "the pid file is owner-only, not {mode:o}"
    );
    let recorded =
        std::fs::read_to_string(temp.join("keeper.pid")).expect("the pid file is readable");
    assert_eq!(
        recorded.trim(),
        keeper.child.id().to_string(),
        "and names this daemon"
    );
}

/// The pid file is published BEFORE the socket, so a worker that can connect can
/// always already find the process it just connected to. The other order leaves
/// a window in which the pid is not yet readable, which is exactly the moment
/// tooling needs it.
#[test]
fn the_pid_file_exists_before_the_socket_is_published() {
    let temp = TempDir::new("ordering");
    let _keeper = Keeper::start(&temp);
    // `start` returns only once the socket is connectable. The pid file being
    // present at that moment is the ordering claim, asserted.
    assert!(
        temp.join("keeper.pid").exists(),
        "the pid file must already be there"
    );
    assert!(temp.join("keeper.sock").exists());
}

/// A clean stop takes the pid file with it, so a stale pid cannot point a
/// service manager at a process that no longer exists.
#[test]
fn a_clean_stop_removes_the_pid_file() {
    let temp = TempDir::new("pidclean");
    let mut keeper = Keeper::start(&temp);
    assert!(temp.join("keeper.pid").exists());
    assert!(keeper.is_running());

    wait_for_close(&temp.join("keeper.sock"));
    let start = Instant::now();
    while keeper.is_running() && start.elapsed() < DEADLINE {
        std::thread::sleep(Duration::from_millis(10));
    }
    let start = Instant::now();
    while temp.join("keeper.pid").exists() && start.elapsed() < DEADLINE {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !temp.join("keeper.pid").exists(),
        "a stopped daemon must not leave a pid behind"
    );
}

/// A clean stop also takes the endpoint, so the next daemon does not have to
/// decide whether the leftover is stale.
#[test]
fn a_clean_stop_removes_the_socket() {
    let temp = TempDir::new("sockclean");
    let _keeper = Keeper::start(&temp);
    assert!(temp.join("keeper.sock").exists());

    wait_for_close(&temp.join("keeper.sock"));
    let start = Instant::now();
    while temp.join("keeper.sock").exists() && start.elapsed() < DEADLINE {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !temp.join("keeper.sock").exists(),
        "a stopped daemon must not leave a socket behind"
    );
}

/// The daemon runs fine with no pid file at all, because a pid file is a
/// convenience for tooling and refusing to own PTYs over one would be worse
/// than running without it.
#[test]
fn a_pid_file_is_optional() {
    let temp = TempDir::new("nopic");
    let socket = temp.join("keeper.sock");
    let child = Command::new(keeper_binary())
        .arg("--socket")
        .arg(&socket)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the keeper binary starts");
    let mut child = child;

    let start = Instant::now();
    while start.elapsed() < STARTUP {
        if socket.exists() && UnixStream::connect(&socket).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(socket.exists(), "the daemon must start without a pid file");
    assert!(
        matches!(child.try_wait(), Ok(None)),
        "and must stay running"
    );

    let _ = child.kill();
    let _ = child.wait();
}

/// A spawn's input still reaches the PTY with no pid file configured, which is
/// the same "it works without it" claim from the other side.
#[test]
fn a_terminal_works_without_a_pid_file() {
    let temp = TempDir::new("nopiterm");
    let socket = temp.join("keeper.sock");
    let mut child = Command::new(keeper_binary())
        .arg("--socket")
        .arg(&socket)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the keeper binary starts");
    let start = Instant::now();
    while start.elapsed() < STARTUP {
        if socket.exists() && UnixStream::connect(&socket).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let request = roost_keeper::frames::SpawnRequest {
        channel_id: 1,
        cols: 80,
        rows: 24,
        shell_spec: roost_keeper::frames::ShellSpec {
            program: "/bin/cat".into(),
            args: Vec::new(),
            env: Vec::new(),
            cwd: None,
        },
    };
    let spawn =
        MuxFrame::json(MuxFrameType::Spawn, 1, &request).expect("a spawn request is small JSON");
    let input = PtyInRequest {
        input_seq: 1,
        bytes: b"no-pid-file\r".to_vec(),
    };
    let input_frame = MuxFrame::new(MuxFrameType::PtyInRequest, 1, input.encode())
        .expect("a small input payload");

    let mut stream = UnixStream::connect(&socket).expect("the keeper is listening");
    stream.write_all(&spawn.encode()).expect("a write");
    stream.write_all(&input_frame.encode()).expect("a write");

    let mut decoder = FrameDecoder::new();
    let mut chunk = vec![0u8; 4096];
    let start = Instant::now();
    let mut echoed = false;
    while start.elapsed() < DEADLINE && !echoed {
        if let Ok(read) = stream.read(&mut chunk) {
            for event in decoder.push(&chunk[..read]) {
                if let StreamEvent::Frame {
                    frame_type: Some(MuxFrameType::PtyOut),
                    payload,
                    ..
                } = event
                    && payload.windows(11).any(|w| w == b"no-pid-file")
                {
                    echoed = true;
                }
            }
        }
    }
    assert!(echoed, "the terminal must work with no pid file configured");

    let _ = child.kill();
    let _ = child.wait();
}
