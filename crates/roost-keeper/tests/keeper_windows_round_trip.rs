#![cfg(windows)]
//! The keeper on Windows, end to end in one process: an AF_UNIX endpoint bound
//! under the temp directory, a client that authenticates, a ConPTY channel
//! running cmd.exe whose output reaches the client, and a kill that ends the
//! channel's job and produces its exit frame. ConPTY opens by asking for the
//! cursor position (`ESC[6n`) and renders nothing until it is answered, which
//! the worker's query-reply lane does in production and this test does by hand.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use roost_keeper::capability::KeeperCapability;
use roost_keeper::client::{KeeperEndpoint, connect};
use roost_keeper::codec::MuxFrameType;
use roost_keeper::frames::ShellSpec;
use roost_keeper::server::{Endpoint, ExitCause, ExitWatch, Server};

static NEVER_SIGNALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

const DEADLINE: Duration = Duration::from_secs(10);

fn fresh_dir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let dir = std::env::temp_dir().join(format!("roost-keeper-win-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a fresh temp directory");
    dir
}

#[test]
fn a_conpty_channel_streams_output_and_its_kill_produces_an_exit() {
    let dir = fresh_dir();
    let socket = dir.join("keeper.sock");
    let capability =
        KeeperCapability::load_or_create(&dir.join("keeper.cap")).expect("a capability");
    let endpoint = Endpoint::new(socket.clone()).expect("a usable endpoint");
    let mut server = Server::bind(endpoint, capability.clone()).expect("the keeper binds");
    let serving = std::thread::spawn(move || {
        if let Some(stream) = server.accept_one() {
            server.serve_one(stream);
        }
        server.keeper_mut().reap_all_channels();
    });

    let client = connect(&KeeperEndpoint { socket, capability }).expect("the client authenticates");
    let spec = ShellSpec {
        program: r"C:\Windows\System32\cmd.exe".to_owned(),
        args: ["/d", "/q", "/k", "echo roost-ready"]
            .map(str::to_owned)
            .to_vec(),
        env: std::env::vars().collect(),
        cwd: None,
    };
    client.spawn(1, spec, 80, 24).expect("the channel opens");

    let mut seen = Vec::new();
    let deadline = Instant::now() + DEADLINE;
    while !String::from_utf8_lossy(&seen).contains("roost-ready") {
        assert!(
            Instant::now() < deadline,
            "no roost-ready within the deadline; saw {:?}",
            String::from_utf8_lossy(&seen)
        );
        if let Some(frame) = client.next_event(Duration::from_millis(100))
            && frame.frame_type == MuxFrameType::PtyOut
            && frame.channel_id == 1
        {
            if frame.payload.windows(4).any(|window| window == b"\x1b[6n") {
                client
                    .write_input(1, b"\x1b[1;1R")
                    .expect("the cursor report is sent");
            }
            seen.extend_from_slice(&frame.payload);
        }
    }

    client.kill(1).expect("the kill is sent");
    let deadline = Instant::now() + DEADLINE;
    loop {
        assert!(
            Instant::now() < deadline,
            "no exit frame within the deadline"
        );
        if let Some(frame) = client.next_event(Duration::from_millis(100))
            && frame.frame_type == MuxFrameType::Exit
            && frame.channel_id == 1
        {
            break;
        }
    }

    drop(client);
    let _ = serving.join();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The socket check reads the AF_UNIX entry itself: a live socket file is a
/// reparse point that following would read as gone, and a keeper that believed
/// that would stop with every PTY in it.
#[test]
fn a_live_socket_file_is_not_read_as_removed() {
    let dir = fresh_dir();
    let socket = dir.join("keeper.sock");
    let capability =
        KeeperCapability::load_or_create(&dir.join("keeper.cap")).expect("a capability");
    let endpoint = Endpoint::new(socket.clone()).expect("a usable endpoint");
    let server = Server::bind(endpoint, capability).expect("the keeper binds");
    let mut watch = ExitWatch::new(&NEVER_SIGNALLED, socket.clone(), Duration::from_millis(1));
    std::thread::sleep(Duration::from_millis(5));
    assert_eq!(watch.poll(), None, "a bound socket is still there");
    drop(server);
    std::thread::sleep(Duration::from_millis(5));
    assert_eq!(watch.poll(), Some(ExitCause::SocketRemoved));
    let _ = std::fs::remove_dir_all(&dir);
}
