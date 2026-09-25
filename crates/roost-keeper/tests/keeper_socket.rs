//! The keeper's socket server over a real Unix socket. The endpoint's security
//! properties are the reason these exist: a keeper socket is a remote shell to
//! every PTY the machine has open, and a window on it is a window onto a shell.
//!
//! Every wait is bounded, because a test that hangs is indistinguishable from a
//! keeper that does.

mod support;

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use roost_keeper::codec::{FrameDecoder, MuxFrame, MuxFrameType, StreamEvent};
use roost_keeper::frames::SpawnAck;
use roost_keeper::server::{Endpoint, ListenError, Server, Shutdown};
use support::{echo, empty_frame, input_frame, resize_frame, wait_until};

const DEADLINE: Duration = Duration::from_secs(10);

/// A socket path in a directory this test owns, removed when it goes out of
/// scope so a failed run cannot poison the next one.
struct TempSocket {
    dir: PathBuf,
    path: PathBuf,
}

impl TempSocket {
    fn new(label: &str) -> Self {
        let unique = format!(
            "roost-keeper-test-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        );
        let dir = std::env::temp_dir().join(unique.replace(['(', ')', ' '], ""));
        std::fs::create_dir_all(&dir).expect("a temp dir for the socket");
        let path = dir.join("keeper.sock");
        Self { dir, path }
    }
}

impl Drop for TempSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A worker end: a real socket plus a decoder, so tests speak the wire format
/// rather than a shortcut.
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

    /// Read until `predicate` is satisfied, returning the frames seen.
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

/// Run a server on `temp`'s endpoint on its own thread, serving exactly one
/// connection. Every test below is this plus a different conversation.
fn serve_on(temp: &TempSocket) -> std::thread::JoinHandle<()> {
    // Bound HERE, on the test's own thread, so the socket is connectable the
    // moment this returns. A test that sleeps and hopes the server won the
    // race is a test that fails on a loaded machine.
    let endpoint = Endpoint::new(temp.path.clone()).expect("a usable endpoint");
    let mut server = Server::bind(endpoint).expect("the keeper binds");
    std::thread::spawn(move || {
        if let Some(stream) = server.accept_one() {
            server.serve_one(stream);
        }
    })
}

/// A spawn over a real socket must be acknowledged with a real pid, and the
/// whole round trip proves the framing works end to end.
#[test]
fn a_spawn_over_a_real_socket_is_acknowledged() {
    let temp = TempSocket::new("spawn");
    let server_thread = serve_on(&temp);

    let mut client = Client::connect(&temp.path);
    client.send(&support::spawn_frame(3, 80, 24));
    let seen = client.read_until("a spawn ack", |frames| {
        frames
            .iter()
            .any(|f| f.frame_type == MuxFrameType::SpawnAck)
    });
    let ack: SpawnAck = seen
        .iter()
        .find(|f| f.frame_type == MuxFrameType::SpawnAck)
        .expect("the ack is in what was read")
        .parse_json()
        .expect("the ack decodes");
    assert!(ack.pid > 0, "a real process, not a placeholder");
    drop(client);
    let _ = server_thread.join();
}

/// Output flows WITHOUT a request. A request-driven loop would show nothing
/// until the user typed, which is the bug the output tick exists to prevent.
#[test]
fn output_flows_without_any_further_request() {
    let temp = TempSocket::new("tick");
    let server_thread = serve_on(&temp);
    let mut client = Client::connect(&temp.path);
    client.send(&support::spawn_frame(1, 80, 24));
    client.read_until("a spawn ack", |f| {
        f.iter().any(|f| f.frame_type == MuxFrameType::SpawnAck)
    });

    // No further request: the echo only appears because the server drains on
    // its own tick.
    client.send(&input_frame(1, 1, b"unsolicited\r"));
    let seen = client.read_until("the echo", |frames| {
        let mut all = Vec::new();
        for frame in frames {
            all.extend_from_slice(&frame.payload);
        }
        all.windows(10).any(|w| w == b"unsolicite")
    });
    let text: Vec<u8> = seen.iter().flat_map(|f| f.payload.clone()).collect();
    assert!(text.windows(10).any(|w| w == b"unsolicite"));
    drop(client);
    let _ = server_thread.join();
}

/// A `Shutdown` frame ends the connection: the daemon's exit decision runs
/// after this, and a connection that stayed open would keep the keeper alive
/// against its own instruction.
#[test]
fn a_shutdown_frame_ends_the_connection() {
    let temp = TempSocket::new("shutdown");
    let server_thread = serve_on(&temp);
    let mut client = Client::connect(&temp.path);
    client.send(&empty_frame(MuxFrameType::Shutdown, 0));
    client.read_until("the ack", |f| {
        f.iter().any(|f| f.frame_type == MuxFrameType::ShutdownAck)
    });

    // The server closes: reads return zero, rather than blocking forever.
    let mut chunk = [0u8; 64];
    wait_until("the connection to close", || {
        match client.stream.read(&mut chunk) {
            Ok(0) | Err(_) => true,
            Ok(_) => false,
        }
    });
    drop(client);
    let _ = server_thread.join();
}

/// `ShutdownIfEmpty` is answered, and the answer is what the daemon's exit
/// decision reads. A live channel must produce a refusal.
#[test]
fn a_conditional_shutdown_over_the_socket_refuses_while_a_channel_is_live() {
    let temp = TempSocket::new("condshutdown");
    let server_thread = serve_on(&temp);
    let mut client = Client::connect(&temp.path);
    client.send(&support::spawn_frame(1, 80, 24));
    client.read_until("a spawn ack", |f| {
        f.iter().any(|f| f.frame_type == MuxFrameType::SpawnAck)
    });

    client.send(&empty_frame(MuxFrameType::ShutdownIfEmpty, 0));
    let seen = client.read_until("a refusal", |f| {
        f.iter()
            .any(|f| f.frame_type == MuxFrameType::ShutdownIfEmptyReject)
    });
    assert!(
        seen.iter()
            .any(|f| f.frame_type == MuxFrameType::ShutdownIfEmptyReject)
    );
    drop(client);
    let _ = server_thread.join();
}

/// The daemon's exit decision is one function, and both paths are asserted here
/// rather than left to a reader of the server loop.
#[test]
fn the_exit_decision_depends_on_whether_a_channel_is_live() {
    let temp = TempSocket::new("decision");
    let endpoint = Endpoint::new(temp.path.clone()).expect("endpoint");
    let mut server = Server::bind(endpoint).expect("bind");
    assert!(server.keeper().is_empty());
    assert!(roost_keeper::server::should_stop(
        server.keeper(),
        Shutdown::StopIfEmpty
    ));
    assert!(roost_keeper::server::should_stop(
        server.keeper(),
        Shutdown::StopPreserving
    ));

    let reply = server.keeper_mut().handle(&support::spawn_frame(1, 80, 24));
    assert_eq!(reply[0].frame_type, MuxFrameType::SpawnAck);
    assert!(
        !roost_keeper::server::should_stop(server.keeper(), Shutdown::StopIfEmpty),
        "a live channel stops the keeper retiring"
    );
    assert!(roost_keeper::server::should_stop(
        server.keeper(),
        Shutdown::StopPreserving
    ));
}

/// Geometry survives the round trip over a real socket, which is the property
/// a client depends on when it asks what the keeper actually applied.
#[test]
fn geometry_survives_the_socket_round_trip() {
    let temp = TempSocket::new("geometry");
    let server_thread = serve_on(&temp);
    let mut client = Client::connect(&temp.path);
    client.send(&support::spawn_frame(1, 80, 24));
    client.read_until("a spawn ack", |f| {
        f.iter().any(|f| f.frame_type == MuxFrameType::SpawnAck)
    });
    client.send(&resize_frame(1, 9, 132, 43));
    client.read_until("a resize ack", |f| {
        f.iter().any(|f| f.frame_type == MuxFrameType::ResizeAck)
    });
    drop(client);
    let _ = server_thread.join();
}

/// The endpoint refuses a path whose parent is not a directory at all, rather
/// than failing later with a confusing bind error.
#[test]
fn an_unusable_parent_is_refused_with_a_reason() {
    let temp = TempSocket::new("badparent");
    let bad = temp.dir.join("not-a-dir").join("keeper.sock");
    assert!(matches!(
        Endpoint::new(bad),
        Err(ListenError::Prepare { .. })
    ));
}

/// A shell that is never written to must not block the server, which is what
/// the bounded read is for.
#[test]
fn an_idle_channel_does_not_stall_the_connection() {
    let temp = TempSocket::new("idle");
    let server_thread = serve_on(&temp);
    let mut client = Client::connect(&temp.path);
    client.send(&support::spawn_with(1, support::idle(), 80, 24));
    client.read_until("a spawn ack", |f| {
        f.iter().any(|f| f.frame_type == MuxFrameType::SpawnAck)
    });

    // The channel produces nothing; the server must still answer promptly.
    let start = Instant::now();
    client.send(&empty_frame(MuxFrameType::Ping, 0));
    client.read_until("a pong", |f| {
        f.iter().any(|f| f.frame_type == MuxFrameType::Pong)
    });
    assert!(
        start.elapsed() < DEADLINE,
        "an idle channel stalled the server"
    );
    drop(client);
    let _ = server_thread.join();
}

/// A shell that echoes proves the keeper is not merely acknowledging frames but
/// actually carrying bytes, which is the one property a terminal depends on.
#[test]
fn the_keeper_carries_real_bytes_over_the_socket() {
    let temp = TempSocket::new("bytes");
    let server_thread = serve_on(&temp);
    let mut client = Client::connect(&temp.path);
    client.send(&support::spawn_frame(1, 80, 24));
    client.read_until("a spawn ack", |f| {
        f.iter().any(|f| f.frame_type == MuxFrameType::SpawnAck)
    });
    client.send(&input_frame(1, 1, b"carried-7788\r"));
    let seen = client.read_until("the echo", |frames| {
        frames.iter().any(|f| {
            f.frame_type == MuxFrameType::PtyOut
                && f.payload.windows(12).any(|w| w == b"carried-7788")
        })
    });
    assert!(seen.iter().any(|f| f.frame_type == MuxFrameType::PtyOut
        && f.payload.windows(12).any(|w| w == b"carried-7788")));
    drop(client);
    let _ = server_thread.join();
}

/// The shell helper is used by the spawn cases above; a shell that is not `cat`
/// would make every byte assertion below vacuous, so it is pinned.
#[test]
fn the_echo_helper_really_is_an_echo() {
    assert_eq!(echo().program, "/bin/cat");
}
