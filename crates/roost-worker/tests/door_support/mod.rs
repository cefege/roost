//! A real loopback door on `127.0.0.1:0` with recording socket owners, and the
//! raw HTTP and WebSocket clients the door tests drive it with. Raw HTTP
//! because the Host header is the thing under test, and a client library
//! writes its own. Shared by `local_door_http`, `local_door_spa` and
//! `local_door_sockets`.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use roost_worker::door::LoopbackSocket;
use roost_worker::door::loopback::{LoopbackHandlers, LoopbackOwner, LoopbackRoutes};
use roost_worker::runtime::door_serve::{DoorConfig, DoorServer, LocalDoor};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;

pub const COORDINATOR_URL: &str = "http://coord.test:4102";
pub const WORKER_FP: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// What a recording owner saw, in order.
#[derive(Debug)]
pub enum DoorEvent {
    Opened(Arc<RecordedPort>),
    Frame(String, Vec<u8>),
    Closed(String),
}

/// The port a recording owner builds: the socket id and the native socket.
#[derive(Debug)]
pub struct RecordedPort {
    pub socket_id: String,
    pub socket: LoopbackSocket,
}

/// A route owner that reports every call on a channel.
pub struct Recording {
    events: mpsc::UnboundedSender<DoorEvent>,
}

impl LoopbackHandlers for Recording {
    type Port = RecordedPort;

    fn port(&self, socket_id: String, socket: LoopbackSocket) -> Arc<RecordedPort> {
        Arc::new(RecordedPort { socket_id, socket })
    }

    fn on_open(&self, port: &Arc<RecordedPort>) -> anyhow::Result<()> {
        let _ = self.events.send(DoorEvent::Opened(Arc::clone(port)));
        Ok(())
    }

    fn on_message(&self, port: &Arc<RecordedPort>, bytes: Vec<u8>) -> anyhow::Result<()> {
        let _ = self
            .events
            .send(DoorEvent::Frame(port.socket_id.clone(), bytes));
        Ok(())
    }

    fn on_close(&self, port: &Arc<RecordedPort>) -> anyhow::Result<()> {
        let _ = self.events.send(DoorEvent::Closed(port.socket_id.clone()));
        Ok(())
    }
}

fn recording() -> (Arc<Recording>, mpsc::UnboundedReceiver<DoorEvent>) {
    let (events, received) = mpsc::unbounded_channel();
    (Arc::new(Recording { events }), received)
}

/// A serving door and what its owners saw.
pub struct TestDoor {
    pub server: DoorServer,
    pub terminal: mpsc::UnboundedReceiver<DoorEvent>,
    pub attachment: Option<mpsc::UnboundedReceiver<DoorEvent>>,
}

impl TestDoor {
    pub fn address(&self) -> SocketAddr {
        self.server.address()
    }

    pub fn origin(&self) -> String {
        self.server.origin()
    }
}

/// How a test door is configured.
pub struct DoorOptions<'a> {
    pub coordinator_url: &'a str,
    pub allowed_browser_origins: &'a [&'a str],
    pub web_dist: Option<&'a Path>,
    pub attachment: bool,
    /// The real terminal owner; a recording one when absent.
    pub terminal: Option<LoopbackOwner>,
    /// A real attachment owner, used as-is (no attachment events recorded).
    pub attachment_owner: Option<LoopbackOwner>,
}

impl Default for DoorOptions<'_> {
    fn default() -> Self {
        Self {
            coordinator_url: COORDINATOR_URL,
            allowed_browser_origins: &[],
            web_dist: None,
            attachment: false,
            terminal: None,
            attachment_owner: None,
        }
    }
}

pub async fn start_door(options: DoorOptions<'_>) -> TestDoor {
    let door = LocalDoor::bind(Some("127.0.0.1:0"))
        .await
        .expect("an ephemeral loopback port is free");
    let (recorder, terminal) = recording();
    let terminal_owner = options
        .terminal
        .unwrap_or_else(|| LoopbackOwner::terminal(recorder));
    let (attachment_owner, attachment) = match (options.attachment_owner, options.attachment) {
        (Some(owner), _) => (Some(owner), None),
        (None, true) => {
            let (owner, events) = recording();
            (Some(LoopbackOwner::attachment(owner)), Some(events))
        }
        (None, false) => (None, None),
    };
    let config = DoorConfig {
        coordinator_url: options.coordinator_url.to_owned(),
        worker_fingerprint: WORKER_FP.to_owned(),
        allowed_browser_origins: options
            .allowed_browser_origins
            .iter()
            .map(|origin| (*origin).to_owned())
            .collect(),
        web_dist: options.web_dist.map(Path::to_path_buf),
    };
    let routes = LoopbackRoutes {
        terminal: terminal_owner,
        attachment: attachment_owner,
    };
    let server = door.serve(&config, routes).expect("the door serves");
    TestDoor {
        server,
        terminal,
        attachment,
    }
}

/// The next thing an owner saw, or a failure naming the wait.
pub async fn next_event(events: &mut mpsc::UnboundedReceiver<DoorEvent>) -> DoorEvent {
    tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("the owner heard something within five seconds")
        .expect("the door is still serving")
}

/// One raw HTTP/1.1 answer.
#[derive(Debug)]
pub struct Answer {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Answer {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(held, _)| held.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Send one request with exactly these headers (plus this door's own Host
/// unless one is given) and read the whole answer.
pub async fn request(
    door: &TestDoor,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
) -> Answer {
    let address = door.address();
    let mut head = format!("{method} {path} HTTP/1.1\r\n");
    if !headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("host"))
    {
        head.push_str(&format!("host: {address}\r\n"));
    }
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("connection: close\r\n\r\n");
    let mut stream = TcpStream::connect(address).await.expect("the door accepts");
    stream
        .write_all(head.as_bytes())
        .await
        .expect("the request is written");
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .expect("the answer is read");
    parse_answer(&raw)
}

fn parse_answer(raw: &[u8]) -> Answer {
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("a complete head");
    let head = String::from_utf8_lossy(&raw[..split]);
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .and_then(|code| code.parse().ok())
        .expect("a status");
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let mut body = raw[split + 4..].to_vec();
    let length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse().ok());
    if let Some(length) = length {
        body.truncate(length);
    }
    Answer {
        status,
        headers,
        body,
    }
}

pub type Client = WebSocketStream<TcpStream>;

/// Open a WebSocket to `path` offering `protocols`; the handshake error when
/// the door refuses it.
pub async fn open_socket(
    door: &TestDoor,
    path: &str,
    protocols: &[&str],
) -> Result<(Client, Option<String>), tokio_tungstenite::tungstenite::Error> {
    let address = door.address();
    let mut request = format!("ws://{address}{path}").into_client_request()?;
    if !protocols.is_empty() {
        request.headers_mut().insert(
            "sec-websocket-protocol",
            protocols.join(", ").parse().expect("a header value"),
        );
    }
    let stream = TcpStream::connect(address).await.expect("the door accepts");
    let (client, response) = tokio_tungstenite::client_async(request, stream).await?;
    let protocol = response
        .headers()
        .get("sec-websocket-protocol")
        .map(|value| value.to_str().expect("an ascii protocol").to_owned());
    Ok((client, protocol))
}
