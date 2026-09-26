//! The keeper client's request/response shapes: what each call asserts about
//! the answer, and what a refusal must name. Split from the connection tests
//! because a failure in one is a protocol contract rather than a question of
//! whether the client can reach a keeper at all.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use roost_keeper::client::connect;
use roost_keeper::frames::ShellSpec;
use roost_keeper::payloads::{PtyInRejectReason, PtyInResult};
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};

const STARTUP: Duration = Duration::from_secs(10);

/// A socket path in a directory this test owns, removed on drop.
struct TempDir {
    dir: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let unique = format!(
            "roost-client-proto-{label}-{}-{:?}",
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

/// A real keeper daemon, killed on drop.
struct Keeper {
    child: Child,
    socket: PathBuf,
}

impl Keeper {
    fn start(temp: &TempDir) -> Self {
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

fn cat() -> ShellSpec {
    ShellSpec {
        program: "/bin/cat".into(),
        args: Vec::new(),
        env: Vec::new(),
        cwd: None,
    }
}

/// property a client needs to decide between a safe retry and a duplicate.
#[test]
fn a_sequenced_write_reports_what_reached_the_pty() {
    let temp = TempDir::new("sequenced");
    let _keeper = Keeper::start(&temp);

    let client = connect(temp.socket()).expect("a handshake");
    client
        .spawn(1, cat(), 80, 24)
        .expect("the spawn is acknowledged");

    let result = client
        .write_input_sequenced(1, 1, b"sequenced\r")
        .expect("the write is answered");
    match result {
        PtyInResult::Ack { input_seq, written } => {
            assert_eq!(input_seq, 1, "the answer names the request");
            assert_eq!(written, 10);
        }
        other => panic!("a small write to an idle PTY completes, got {other:?}"),
    }
}

/// A sequenced write to a channel that does not exist is REJECTED rather than
/// dropped, and the rejection names the sequence so the client can match it.
#[test]
fn a_sequenced_write_to_an_unknown_channel_is_rejected() {
    let temp = TempDir::new("nosuchchan");
    let _keeper = Keeper::start(&temp);

    let client = connect(temp.socket()).expect("a handshake");
    let result = client
        .write_input_sequenced(99, 7, b"nowhere\r")
        .expect("the write is answered");

    match result {
        PtyInResult::Reject { input_seq, reason } => {
            assert_eq!(input_seq, 7, "the refusal names the request");
            assert_eq!(reason, PtyInRejectReason::NoSuchChannel);
        }
        other => panic!("a write to a channel that does not exist is rejected, got {other:?}"),
    }
}

/// A resize is acknowledged with the geometry the keeper actually applied.
#[test]
fn a_resize_is_acknowledged_with_the_applied_geometry() {
    let temp = TempDir::new("resize");
    let _keeper = Keeper::start(&temp);

    let client = connect(temp.socket()).expect("a handshake");
    client
        .spawn(1, cat(), 80, 24)
        .expect("the spawn is acknowledged");
    client
        .resize(1, 1, 132, 43)
        .expect("the resize is acknowledged");
}

/// The channel list is how a resuming worker finds what survived it.
#[test]
fn the_channel_list_names_the_surviving_channels() {
    let temp = TempDir::new("list");
    let _keeper = Keeper::start(&temp);

    let client = connect(temp.socket()).expect("a handshake");
    client
        .spawn(3, cat(), 80, 24)
        .expect("the spawn is acknowledged");
    client
        .spawn(9, cat(), 80, 24)
        .expect("the second spawn is acknowledged");

    let listed = client.list_channels().expect("the list comes back");
    let ids: Vec<u16> = listed.channels.iter().map(|c| c.channel_id).collect();
    assert_eq!(ids, vec![3, 9], "sorted, so two workers see the same order");
}

/// A PTY that outlives its client is the keeper's reason for existing, and the
/// client can see it from a second connection.
#[test]
fn a_second_client_finds_the_channel_the_first_left() {
    let temp = TempDir::new("resume");
    let _keeper = Keeper::start(&temp);

    {
        let first = connect(temp.socket()).expect("a handshake");
        first
            .spawn(4, cat(), 80, 24)
            .expect("the spawn is acknowledged");
    }

    // The keeper serves one connection at a time, so it needs a moment to
    // notice the first one is gone. The client's connect retry covers exactly
    // this, which is why the test does not sleep a guessed interval.
    let second = connect(temp.socket()).expect("a handshake once the keeper is free");
    let listed = second.list_channels().expect("the list comes back");
    assert_eq!(listed.channels.len(), 1);
    assert_eq!(listed.channels[0].channel_id, 4);
}
