//! The keeper binary, run as a real process against a real socket. Everything
//! here is about the things a library test cannot show: that the daemon binds,
//! that it keeps running when a worker goes away, and that it stops when it is
//! told to.
//!
//! The one that matters most is `a_worker_disconnect_does_not_stop_the_keeper`:
//! the keeper exists so a worker restart costs a reconnect, and a daemon that
//! exited on disconnect would have defeated the entire design while passing
//! every unit test in the crate.

mod support;

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use roost_keeper::codec::{FrameDecoder, MuxFrame, MuxFrameType, StreamEvent};

use support::{empty_frame, spawn_frame};

const DEADLINE: Duration = Duration::from_secs(15);
const STARTUP: Duration = Duration::from_secs(10);

struct TempDir {
    dir: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let unique = format!(
            "roost-keeper-bin-{label}-{}-{:?}",
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

/// The keeper binary, built by cargo as part of this test run.
fn keeper_binary() -> PathBuf {
    // `CARGO_BIN_EXE_<name>` is set by cargo for integration tests of a crate
    // with binaries, so this is the binary cargo just built rather than a
    // guess at where it landed.
    PathBuf::from(env!("CARGO_BIN_EXE_roost-keeper"))
}

/// A running keeper, killed on drop so a failed test cannot leave one behind.
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

    fn connect(&self) -> Client {
        Client::connect(&self.socket)
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

struct Client {
    stream: UnixStream,
    decoder: FrameDecoder,
}

impl Client {
    fn connect(path: &Path) -> Self {
        let stream = UnixStream::connect(path).expect("the keeper is listening");
        Self {
            stream,
            decoder: FrameDecoder::new(),
        }
    }

    fn send(&mut self, frame: &MuxFrame) {
        self.stream
            .write_all(&frame.encode())
            .expect("a write to a live socket");
    }

    fn read_until(
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
}

fn wait_for(mut predicate: impl FnMut() -> bool, what: &str) {
    let start = Instant::now();
    while start.elapsed() < DEADLINE {
        if predicate() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("{what} never happened within {DEADLINE:?}");
}

/// The daemon binds its endpoint and answers a real spawn. This is the whole
/// binary working, end to end, as a separate process.
#[test]
fn the_daemon_binds_and_serves_a_real_spawn() {
    let temp = TempDir::new("serve");
    let mut keeper = Keeper::start(&temp);

    let mut client = keeper.connect();
    client.send(&spawn_frame(1, 80, 24));
    client.read_until("a spawn ack", |frames| {
        frames
            .iter()
            .any(|f| f.frame_type == MuxFrameType::SpawnAck)
    });
    assert!(keeper.is_running());
}

/// THE PROPERTY. A worker going away must NOT take the keeper with it: the
/// keeper exists so a worker restart costs a reconnect, not a terminal. A
/// daemon that exited here would have defeated the design while passing every
/// unit test in the crate.
#[test]
fn a_worker_disconnect_does_not_stop_the_keeper() {
    let temp = TempDir::new("disconnect");
    let mut keeper = Keeper::start(&temp);

    {
        let mut client = keeper.connect();
        client.send(&spawn_frame(7, 80, 24));
        client.read_until("a spawn ack", |frames| {
            frames
                .iter()
                .any(|f| f.frame_type == MuxFrameType::SpawnAck)
        });
        // The connection drops here, with a live channel.
    }

    std::thread::sleep(Duration::from_millis(200));
    assert!(
        keeper.is_running(),
        "a disconnect is not a shutdown; the keeper must outlive the worker"
    );
    assert!(keeper.socket.exists(), "and its endpoint must survive too");
}

/// A second worker must find the channel the first one left, which is the
/// cross-process resume the whole `ListChannels` frame exists for.
#[test]
fn a_reconnecting_worker_finds_the_surviving_channel() {
    let temp = TempDir::new("resume");
    let keeper = Keeper::start(&temp);

    {
        let mut first = keeper.connect();
        first.send(&spawn_frame(5, 80, 24));
        first.read_until("a spawn ack", |frames| {
            frames
                .iter()
                .any(|f| f.frame_type == MuxFrameType::SpawnAck)
        });
    }

    let mut second = keeper.connect();
    second.send(&empty_frame(MuxFrameType::ListChannels, 0));
    let seen = second.read_until("the channel list", |frames| {
        frames
            .iter()
            .any(|f| f.frame_type == MuxFrameType::ListChannelsResp)
    });
    let listed: roost_keeper::frames::ListChannelsResp = seen
        .iter()
        .find(|f| f.frame_type == MuxFrameType::ListChannelsResp)
        .expect("the list is in what was read")
        .parse_json()
        .expect("it decodes");
    assert_eq!(
        listed.channels.len(),
        1,
        "the PTY survived the worker that spawned it: {:?}",
        listed.channels
    );
    assert_eq!(listed.channels[0].channel_id, 5);
}

/// A conditional shutdown with a live channel must leave the daemon running.
#[test]
fn a_conditional_shutdown_with_a_live_channel_keeps_the_daemon_up() {
    let temp = TempDir::new("condlive");
    let mut keeper = Keeper::start(&temp);

    let mut client = keeper.connect();
    client.send(&spawn_frame(1, 80, 24));
    client.read_until("a spawn ack", |frames| {
        frames
            .iter()
            .any(|f| f.frame_type == MuxFrameType::SpawnAck)
    });
    client.send(&empty_frame(MuxFrameType::ShutdownIfEmpty, 0));
    client.read_until("a refusal", |frames| {
        frames
            .iter()
            .any(|f| f.frame_type == MuxFrameType::ShutdownIfEmptyReject)
    });

    std::thread::sleep(Duration::from_millis(200));
    assert!(
        keeper.is_running(),
        "a refusal means the keeper holds live PTYs, so it must not retire"
    );
}

/// A conditional shutdown on an empty keeper must stop the daemon. This is the
/// automatic boot-replacement path, and it only works if the exit decision
/// actually reads the answer rather than the request.
#[test]
fn a_conditional_shutdown_on_an_empty_keeper_stops_the_daemon() {
    let temp = TempDir::new("condempty");
    let mut keeper = Keeper::start(&temp);

    let mut client = keeper.connect();
    client.send(&empty_frame(MuxFrameType::ShutdownIfEmpty, 0));
    client.read_until("an ack", |frames| {
        frames
            .iter()
            .any(|f| f.frame_type == MuxFrameType::ShutdownIfEmptyAck)
    });

    wait_for(|| !keeper.is_running(), "the daemon to stop");
}

/// An unconditional shutdown stops the daemon, because it is the deliberate
/// offline maintenance path and it does not consult the channel table.
#[test]
fn an_unconditional_shutdown_stops_the_daemon() {
    let temp = TempDir::new("uncond");
    let mut keeper = Keeper::start(&temp);

    let mut client = keeper.connect();
    client.send(&empty_frame(MuxFrameType::Shutdown, 0));
    client.read_until("an ack", |frames| {
        frames
            .iter()
            .any(|f| f.frame_type == MuxFrameType::ShutdownAck)
    });

    wait_for(|| !keeper.is_running(), "the daemon to stop");
}

/// A real terminal works through the daemon: bytes go in, bytes come back. This
/// is the one property the whole program exists to provide.
#[test]
fn a_real_terminal_works_through_the_daemon() {
    let temp = TempDir::new("terminal");
    let keeper = Keeper::start(&temp);

    let mut client = keeper.connect();
    client.send(&spawn_frame(1, 100, 30));
    client.read_until("a spawn ack", |frames| {
        frames
            .iter()
            .any(|f| f.frame_type == MuxFrameType::SpawnAck)
    });
    client.send(&support::input_frame(1, 1, b"through-the-daemon\r"));
    client.read_until("the echo", |frames| {
        frames.iter().any(|f| {
            f.frame_type == MuxFrameType::PtyOut
                && f.payload.windows(18).any(|w| w == b"through-the-daemon")
        })
    });
}

/// The spawn ack carries the CHILD's pid, not the daemon's, which is what
/// lets an operator find the process a channel is attached to. A daemon that
/// reported its own pid here would send every operator to the wrong process.
#[test]
fn a_spawn_ack_carries_a_real_child_pid() {
    let temp = TempDir::new("childpid");
    let mut keeper = Keeper::start(&temp);
    let daemon_pid = keeper.child.id();
    assert!(keeper.is_running());

    let mut client = keeper.connect();
    client.send(&support::spawn_frame(1, 80, 24));
    let seen = client.read_until("a spawn ack", |frames| {
        frames
            .iter()
            .any(|f| f.frame_type == MuxFrameType::SpawnAck)
    });
    let ack: roost_keeper::frames::SpawnAck = seen
        .iter()
        .find(|f| f.frame_type == MuxFrameType::SpawnAck)
        .expect("the ack is in what was read")
        .parse_json()
        .expect("it decodes");
    assert_ne!(ack.pid, 0);
    assert_ne!(
        ack.pid,
        std::process::id(),
        "the child is not this test process"
    );
    assert_ne!(ack.pid, daemon_pid, "and not the daemon either");
}
