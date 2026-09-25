//! How the keeper behaves over a real socket when the conversation is about
//! the PROTOCOL rather than about a terminal: a tag it predates, a shutdown, an
//! exit. Split from the byte-carrying cases because a failure here is a framing
//! or lifecycle bug, not a terminal bug.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use roost_keeper::codec::{FrameDecoder, MuxFrame, MuxFrameType, StreamEvent};
use roost_keeper::frames::ExitFrame;
use roost_keeper::server::{Endpoint, Server};

mod support;

use support::empty_frame;

const DEADLINE: Duration = Duration::from_secs(10);

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

struct Client {
    stream: std::os::unix::net::UnixStream,
    decoder: FrameDecoder,
}

impl Client {
    fn connect(path: &Path) -> Self {
        let stream =
            std::os::unix::net::UnixStream::connect(path).expect("the keeper is listening");
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

fn serve_on(temp: &TempSocket) -> std::thread::JoinHandle<()> {
    let endpoint = Endpoint::new(temp.path.clone()).expect("a usable endpoint");
    let mut server = Server::bind(endpoint).expect("the keeper binds");
    std::thread::spawn(move || {
        if let Some(stream) = server.accept_one() {
            server.serve_one(stream);
        }
    })
}

/// An unknown tag must not end the connection. It is how a newer keeper says it
/// has a frame this build predates, and the length has already been read, so
/// skipping it is safe and dropping a live terminal over it is not.
#[test]
fn an_unknown_tag_does_not_end_the_connection() {
    let temp = TempSocket::new("unknowntag");
    let server_thread = serve_on(&temp);
    let mut client = Client::connect(&temp.path);
    // A well-formed frame carrying a tag this build has never heard of.
    let mut raw = Vec::new();
    raw.extend_from_slice(&4u32.to_be_bytes());
    raw.push(0xEF);
    raw.extend_from_slice(&0u16.to_be_bytes());
    raw.push(0xAA);
    client.stream.write_all(&raw).expect("a write");

    // The connection still works afterwards.
    client.send(&empty_frame(MuxFrameType::Ping, 0));
    client.read_until("a pong", |f| {
        f.iter().any(|f| f.frame_type == MuxFrameType::Pong)
    });
    drop(client);
    let _ = server_thread.join();
}

/// An exit is reported over the socket once the channel's output has drained,
/// so a client never loses the last thing a process printed.
#[test]
fn an_exit_is_reported_over_the_socket() {
    let temp = TempSocket::new("exit");
    let server_thread = serve_on(&temp);
    let mut client = Client::connect(&temp.path);
    client.send(&support::spawn_with(
        1,
        support::running("echo farewell; exit 5"),
        80,
        24,
    ));
    client.read_until("a spawn ack", |f| {
        f.iter().any(|f| f.frame_type == MuxFrameType::SpawnAck)
    });

    let seen = client.read_until("the exit", |frames| {
        let said = frames.iter().any(|f| {
            f.frame_type == MuxFrameType::PtyOut && f.payload.windows(8).any(|w| w == b"farewell")
        });
        said && frames.iter().any(|f| f.frame_type == MuxFrameType::Exit)
    });
    let exit_frame = seen
        .iter()
        .find(|f| f.frame_type == MuxFrameType::Exit)
        .expect("the exit is in what was read");
    let exit: ExitFrame = exit_frame.parse_json().expect("it decodes");
    assert_eq!(exit.exit_code, Some(5));
    drop(client);
    let _ = server_thread.join();
}
