//! The door's one WebSocket pump for both loopback routes, and the handler
//! contract each route's owner implements (v2 `local-door/local-ui-server.ts`
//! `websocket.open/message/close` and `guarded`, with the handler shapes of
//! `local-ui-terminal-socket.ts` and `attachments/local-ui-attachment-socket.ts`).
//! `runtime::door_routes` upgrades a socket and hands it here; the terminal and
//! attachment owners see only [`LoopbackHandlers`] calls, in frame order.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use futures_util::StreamExt as _;
use futures_util::future::BoxFuture;
use tokio::sync::watch;
use tokio::time::{Instant, sleep};

use super::LOCAL_ATTACHMENT_MAX_PAYLOAD_BYTES;
use super::loopback_socket::LoopbackSocket;

/// Bun's `ServerWebSocket` default `idleTimeout` with its automatic pings,
/// which v2 relied on to reap a peer that stops answering without a FIN: a
/// silent socket is pinged once, and closed if it stays silent as long again.
const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// What a loopback route's owner does with its sockets (v2
/// `LocalTerminalSocketHandlers` / `LocalAttachmentSocketHandlers`, plus the
/// port adapter the server wraps each native socket in).
///
/// Every call comes from the socket's own task, in frame order, never from
/// inside a [`LoopbackSocket`] call. An `Err` is v2's throw out of `guarded`:
/// it costs that socket (closed 1011), never the worker. `on_close` runs exactly
/// once for every port built, after the socket has ended for any reason.
pub trait LoopbackHandlers: Send + Sync + 'static {
    /// The route's port adapter.
    type Port: Send + Sync + 'static;

    /// Wrap a freshly upgraded socket in this route's port.
    fn port(&self, socket_id: String, socket: LoopbackSocket) -> Arc<Self::Port>;
    /// The socket is open.
    fn on_open(&self, port: &Arc<Self::Port>) -> anyhow::Result<()>;
    /// One binary frame from the browser.
    fn on_message(&self, port: &Arc<Self::Port>, bytes: Vec<u8>) -> anyhow::Result<()>;
    /// The socket has ended.
    fn on_close(&self, port: &Arc<Self::Port>) -> anyhow::Result<()>;
}

/// Which loopback route a socket arrived on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopbackRoute {
    Terminal,
    Attachment,
}

impl LoopbackRoute {
    /// The route's name in a refusal reason (`terminal_subprotocol`, ...).
    pub fn name(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::Attachment => "attachment",
        }
    }

    /// The one subprotocol the route upgrades.
    pub fn subprotocol(self) -> &'static str {
        match self {
            Self::Terminal => super::LOCAL_TERMINAL_SUBPROTOCOL,
            Self::Attachment => super::LOCAL_ATTACHMENT_SUBPROTOCOL,
        }
    }
}

type ServeSocket =
    dyn Fn(String, WebSocket, watch::Receiver<bool>) -> BoxFuture<'static, ()> + Send + Sync;

/// A route's owner, type-erased so the door holds either route alike.
#[derive(Clone)]
pub struct LoopbackOwner {
    route: LoopbackRoute,
    serve: Arc<ServeSocket>,
}

impl std::fmt::Debug for LoopbackOwner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LoopbackOwner")
            .field("route", &self.route)
            .finish_non_exhaustive()
    }
}

impl LoopbackOwner {
    /// The owner of `/ws/local-terminal` sockets.
    pub fn terminal<H: LoopbackHandlers>(handlers: Arc<H>) -> Self {
        Self::for_route(LoopbackRoute::Terminal, handlers)
    }

    /// The owner of attachment transfer sockets.
    pub fn attachment<H: LoopbackHandlers>(handlers: Arc<H>) -> Self {
        Self::for_route(LoopbackRoute::Attachment, handlers)
    }

    fn for_route<H: LoopbackHandlers>(route: LoopbackRoute, handlers: Arc<H>) -> Self {
        let serve = move |socket_id: String,
                          websocket: WebSocket,
                          stop: watch::Receiver<bool>|
              -> BoxFuture<'static, ()> {
            Box::pin(pump(
                route,
                Arc::clone(&handlers),
                socket_id,
                websocket,
                stop,
            ))
        };
        Self {
            route,
            serve: Arc::new(serve),
        }
    }

    /// The route this owner serves.
    pub fn route(&self) -> LoopbackRoute {
        self.route
    }

    /// Run one upgraded socket until it ends or the door stops.
    pub(crate) fn serve_socket(
        &self,
        socket_id: String,
        websocket: WebSocket,
        stop: watch::Receiver<bool>,
    ) -> BoxFuture<'static, ()> {
        (self.serve)(socket_id, websocket, stop)
    }
}

/// The owners the door upgrades sockets into (v2 `LocalUiServerDeps.terminal`
/// and `.attachment`). An absent attachment owner answers v2's 404.
#[derive(Debug, Clone)]
pub struct LoopbackRoutes {
    pub terminal: LoopbackOwner,
    pub attachment: Option<LoopbackOwner>,
}

/// Resolves once the door is told to stop, or once the door itself is gone.
pub(crate) async fn door_stopped(stop: &mut watch::Receiver<bool>) {
    let _ = stop.wait_for(|stopped| *stopped).await;
}

/// Why a socket's pump ended, for its close log line.
#[derive(Debug, Clone, Copy)]
enum Ended {
    PeerGone,
    Closed,
    Idle,
    DoorStopped,
}

async fn pump<H: LoopbackHandlers>(
    route: LoopbackRoute,
    handlers: Arc<H>,
    socket_id: String,
    websocket: WebSocket,
    mut stop: watch::Receiver<bool>,
) {
    let (sink, mut inbound) = websocket.split();
    let (socket, outbound) = LoopbackSocket::open();
    let mut writer = tokio::spawn(outbound.drain_into(sink));
    let port = handlers.port(socket_id.clone(), socket.clone());
    match route {
        LoopbackRoute::Terminal => tracing::info!(socket_id, "local_terminal_socket_opened"),
        LoopbackRoute::Attachment => tracing::info!(socket_id, "local_attachment_socket_opened"),
    }
    guarded(&socket, &socket_id, "open", handlers.on_open(&port));

    let idle = sleep(IDLE_TIMEOUT);
    tokio::pin!(idle);
    let mut pinged = false;
    let ended = loop {
        tokio::select! {
            () = door_stopped(&mut stop) => break Ended::DoorStopped,
            _ = &mut writer => break Ended::Closed,
            () = &mut idle => {
                if pinged {
                    break Ended::Idle;
                }
                pinged = true;
                socket.ping();
                idle.as_mut().reset(Instant::now() + IDLE_TIMEOUT);
            }
            frame = inbound.next() => {
                pinged = false;
                idle.as_mut().reset(Instant::now() + IDLE_TIMEOUT);
                match frame {
                    Some(Ok(Message::Binary(bytes))) if socket.is_open() => {
                        deliver(route, &*handlers, &port, &socket, &socket_id, Vec::from(bytes));
                    }
                    Some(Ok(Message::Text(_))) => tracing::warn!(
                        socket_id,
                        reason = "non_binary_frame",
                        "local_direct_socket_rejected"
                    ),
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => break Ended::PeerGone,
                }
            }
        }
    };
    socket.mark_closed();
    writer.abort();
    match route {
        LoopbackRoute::Terminal => {
            tracing::info!(socket_id, ?ended, "local_terminal_socket_closed")
        }
        LoopbackRoute::Attachment => {
            tracing::info!(socket_id, ?ended, "local_attachment_socket_closed");
        }
    }
    guarded(&socket, &socket_id, "close", handlers.on_close(&port));
}

/// One binary frame to the route's owner, after the attachment route's size
/// gate (v2 refuses an oversized attachment frame before any decoding).
fn deliver<H: LoopbackHandlers>(
    route: LoopbackRoute,
    handlers: &H,
    port: &Arc<H::Port>,
    socket: &LoopbackSocket,
    socket_id: &str,
    bytes: Vec<u8>,
) {
    if route == LoopbackRoute::Attachment && bytes.len() > LOCAL_ATTACHMENT_MAX_PAYLOAD_BYTES {
        tracing::warn!(
            socket_id,
            reason = "attachment_frame_too_large",
            "local_direct_socket_rejected"
        );
        socket.close(1009, "attachment frame too large");
        return;
    }
    guarded(
        socket,
        socket_id,
        "message",
        handlers.on_message(port, bytes),
    );
}

/// v2 `guarded`: a failure out of a handler costs that socket, never the worker.
fn guarded(
    socket: &LoopbackSocket,
    socket_id: &str,
    stage: &'static str,
    outcome: anyhow::Result<()>,
) {
    if let Err(error) = outcome {
        tracing::error!(socket_id, reason = stage, %error, "local_direct_socket_rejected");
        socket.close(1011, "local direct handler failed");
    }
}
