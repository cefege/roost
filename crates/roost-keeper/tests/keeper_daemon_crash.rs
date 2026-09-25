//! What a keeper daemon that DIED leaves behind, and what the next one makes of
//! it. Split from the lifecycle tests because a crash is a different event from
//! a clean stop, and the leftovers are a different kind of problem: a clean
//! stop cleans up after itself, and the interesting case is the one where it
//! could not.

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const STARTUP: Duration = Duration::from_secs(10);
const DEADLINE: Duration = Duration::from_secs(15);

struct TempDir {
    dir: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let unique = format!(
            "roost-keeper-crash-{label}-{}-{:?}",
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

fn spawn_daemon(socket: &std::path::Path) -> Child {
    Command::new(keeper_binary())
        .arg("--socket")
        .arg(socket)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the keeper binary starts")
}

fn wait_until_listening(socket: &std::path::Path) {
    let start = Instant::now();
    while start.elapsed() < STARTUP {
        if socket.exists() && UnixStream::connect(socket).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("the keeper never started listening on {}", socket.display());
}

/// be a permanent "address in use".
#[test]
fn a_daemon_that_died_leaves_a_socket_the_next_one_reclaims() {
    let temp = TempDir::new("crash");
    let socket = temp.join("keeper.sock");
    {
        let mut child = spawn_daemon(&socket);
        wait_until_listening(&socket);
        // SIGKILL leaves the socket file exactly as a crash does.
        let pid = child.id() as i32;
        // SAFETY: `kill` with a valid pid and SIGKILL is the documented use, and
        // the child is this test's own.
        unsafe { libc::kill(pid, libc::SIGKILL) };
        let _ = child.wait();
    }
    assert!(socket.exists(), "a killed daemon leaves its socket file");

    let mut replacement = spawn_daemon(&socket);
    wait_until_listening(&socket);
    assert!(
        matches!(replacement.try_wait(), Ok(None)),
        "and the next daemon must take it over"
    );
    let _ = replacement.kill();
    let _ = replacement.wait();
}

/// A signal stops the daemon, which is how a service manager and a deploy
/// retire it. `SIGTERM` is the one that must be graceful.
#[test]
fn a_signal_stops_the_daemon() {
    let temp = TempDir::new("signal");
    let mut child = spawn_daemon(&temp.join("keeper.sock"));
    wait_until_listening(&temp.join("keeper.sock"));

    let pid = child.id() as i32;
    // SAFETY: `kill` with a valid pid and SIGTERM is the documented use. The
    // child is this test's own, spawned above.
    let sent = unsafe { libc::kill(pid, libc::SIGTERM) };
    assert_eq!(sent, 0, "the signal was delivered");

    let start = Instant::now();
    while matches!(child.try_wait(), Ok(None)) && start.elapsed() < DEADLINE {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !matches!(child.try_wait(), Ok(None)),
        "SIGTERM must stop the daemon"
    );
    let _ = child.kill();
    let _ = child.wait();
}
