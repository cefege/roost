//! The coordinator link's dial: the URL, the credential, and the per-dial
//! lifecycle. Owned by the worker.
//!
//! The transport underneath is a raw WebSocket carrying the coordinator proto
//! frames as binary messages, not a generated bidi client: h2 is incomplete in
//! the runtime and h1.1 buffers the upstream, so the worker's replies stalled
//! and `sessionsSpawn` hung behind them.
//!
//! Everything policy-shaped — when to retry, when to escalate — is in
//! [`crate::backoff`]. This file is the transport and the lifecycle around it.

use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// The subprotocol that carries the worker's credential.
///
/// A CREDENTIAL, never URL material. A JWT in a path lands in proxy access
/// logs, in any error page that echoes the URL, and in whatever the client
/// library decides to log. The fingerprint goes in the path because the
/// coordinator routes on it; the credential does not, because the coordinator
/// already knows who is dialling.
pub const WORKER_AUTH_SUBPROTOCOL: &str = "roost-worker-auth";

/// The path a worker dials, as a format over its fingerprint.
pub const WORKER_DIAL_PATH: &str = "/ws/coord-worker/{fingerprint}";

/// Why a dial did not produce an open link.
///
/// The distinction is the one the backoff policy is built on: a dial that never
/// opened is counted as a possible auth rejection, while a link that opened and
/// then dropped is an ordinary reconnect.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DialError {
    #[error("no credential is available for this dial")]
    NoCredential,
    #[error("{base} is not a usable coordinator base: {reason}")]
    BadBase { base: String, reason: String },
    #[error("the coordinator did not accept the upgrade: {reason}")]
    NotAccepted { reason: String },
    #[error("the link opened and then ended: {reason}")]
    OpenedThenClosed { reason: String },
}

/// The coordinator endpoint a worker dials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoordinatorEndpoint {
    /// Rewritten to a `ws` scheme, with no trailing slash.
    base: String,
    /// This worker's registry fingerprint. In the path, because the coordinator
    /// routes on it.
    fingerprint: String,
}

impl CoordinatorEndpoint {
    /// Build an endpoint, rejecting a base that cannot be dialled.
    ///
    /// An `http` base is rewritten to `ws` and an `https` one to `wss`,
    /// because an operator configuring a worker thinks in terms of the
    /// coordinator's other URLs and should not have to know which scheme this
    /// particular endpoint wants.
    pub fn new(base: impl Into<String>, fingerprint: impl Into<String>) -> Result<Self, DialError> {
        let base = base.into();
        let fingerprint = fingerprint.into();
        if fingerprint.is_empty() || fingerprint.contains('/') {
            return Err(DialError::BadBase {
                base: base.clone(),
                reason: format!("the fingerprint {fingerprint:?} is not a path segment"),
            });
        }
        let rewritten = match base.split_once("://") {
            Some(("http", rest)) => format!("ws://{rest}"),
            Some(("https", rest)) => format!("wss://{rest}"),
            Some(("ws" | "wss", _)) => base.clone(),
            _ => {
                return Err(DialError::BadBase {
                    base: base.clone(),
                    reason: "no recognised scheme".to_string(),
                });
            }
        };
        Ok(Self {
            base: rewritten.trim_end_matches('/').to_string(),
            fingerprint,
        })
    }

    /// The full dial URL, for logs and for the request.
    pub fn url(&self) -> String {
        format!(
            "{}{}",
            self.base,
            WORKER_DIAL_PATH.replace("{fingerprint}", &self.fingerprint)
        )
    }
}

/// One dial's lifecycle, so `open` and `closed` happen exactly once each.
///
/// A dial produces several terminal events — a construction failure, an
/// upgrade error, a close — and the v2 bug this replaces was cleanup running
/// more than once for one of them. Making the lifecycle explicit means the
/// second call is observably a no-op rather than a double teardown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialPhase {
    /// Constructed but not yet open. Sends must NOT touch the socket: they
    /// belong in the outbox, which is bounded, until there is something to
    /// write to.
    Connecting,
    /// Open and usable.
    Open,
    /// Finished, whether by failure or by a close.
    Closed,
}

/// An open coordinator link.
pub struct Link {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    /// What the server selected during the handshake. Captured from the upgrade
    /// response, because the stream does not retain it.
    negotiated: Option<String>,
    phase: DialPhase,
}

impl std::fmt::Debug for Link {
    /// Deliberately omits the socket: a WebSocket's debug output is its whole
    /// frame buffer, and this type appears in `roost doctor` output.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Link")
            .field("phase", &self.phase)
            .field("negotiated", &self.negotiated)
            .finish()
    }
}

impl Link {
    /// Wrap an established socket and the subprotocol it negotiated.
    pub fn new(
        socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
        negotiated: Option<String>,
    ) -> Self {
        Self {
            socket,
            negotiated,
            phase: DialPhase::Open,
        }
    }

    pub fn phase(&self) -> DialPhase {
        self.phase
    }

    /// Mark the link finished.
    ///
    /// Idempotent, and reports whether THIS call was the one that closed it —
    /// which is what lets a caller feed the backoff exactly one event per dial.
    pub fn close(&mut self) -> bool {
        if self.phase == DialPhase::Closed {
            return false;
        }
        self.phase = DialPhase::Closed;
        true
    }

    /// Send one binary frame upstream.
    pub async fn send(&mut self, bytes: Vec<u8>) -> Result<(), DialError> {
        self.socket
            .send(Message::Binary(bytes.into()))
            .await
            .map_err(|err| DialError::OpenedThenClosed {
                reason: err.to_string(),
            })
    }

    /// Take the next downstream frame.
    ///
    /// `None` means the link ENDED; `Some(Err(_))` means a frame arrived and
    /// could not be handled. The caller reconnects on the first and reconnects
    /// nothing on the second — collapsing them would turn a protocol error into
    /// a reconnect loop, which is the shape of a coordinator flapping for no
    /// reason.
    pub async fn recv(&mut self) -> Option<Result<Message, String>> {
        self.socket
            .next()
            .await
            .map(|result| result.map_err(|err| err.to_string()))
    }

    /// The subprotocol the server selected, if any.
    ///
    /// A server that selects none has authenticated nothing, so this is what a
    /// caller checks before trusting a link. A link that is open but did not
    /// negotiate is not authenticated, and must not carry a credential.
    pub fn negotiated_subprotocol(&self) -> Option<&str> {
        self.negotiated.as_deref()
    }
}

/// Build the upgrade request for a dial.
///
/// The credential is the SECOND requested subprotocol and the fingerprint is
/// the path. A request that put the credential in the path would be exactly the
/// regression this crate exists to prevent.
pub fn dial_request(
    endpoint: &CoordinatorEndpoint,
    credential: &str,
) -> Result<tokio_tungstenite::tungstenite::http::Request<()>, DialError> {
    if credential.is_empty() {
        return Err(DialError::NoCredential);
    }
    let mut request =
        endpoint
            .url()
            .into_client_request()
            .map_err(|err| DialError::NotAccepted {
                reason: err.to_string(),
            })?;
    // Ordering matters: the first entry is what a server without auth support
    // will echo, so the marker comes first and the secret second.
    let protocols = format!("{WORKER_AUTH_SUBPROTOCOL}, {credential}");
    let header = HeaderValue::from_str(&protocols).map_err(|err| DialError::NotAccepted {
        reason: err.to_string(),
    })?;
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", header);
    Ok(request)
}

/// Dial the coordinator once.
///
/// One attempt. The retry loop is the caller's, because the backoff policy
/// lives in [`crate::backoff`] and a dial that owned its own retries would make
/// that policy untestable without a network.
pub async fn dial(
    endpoint: &CoordinatorEndpoint,
    credential: &str,
    timeout: Duration,
) -> Result<Link, DialError> {
    let request = dial_request(endpoint, credential)?;
    // No TLS connector is configured. A `wss` endpoint therefore fails the
    // upgrade rather than silently downgrading, which is the safe direction to
    // fail in: a worker that quietly loses TLS to a coordinator that thinks it
    // is encrypted is worse than one that refuses to connect.
    let attempt = tokio_tungstenite::connect_async_with_config(request, None, false);
    match tokio::time::timeout(timeout, attempt).await {
        Ok(Ok((socket, response))) => {
            let negotiated = response
                .headers()
                .get("sec-websocket-protocol")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            Ok(Link::new(socket, negotiated))
        }
        // A timeout here is a NON-OPEN dial, which is what the escalation
        // counts. It is not an auth rejection and is not reported as one.
        Ok(Err(err)) => Err(DialError::NotAccepted {
            reason: err.to_string(),
        }),
        Err(_) => Err(DialError::NotAccepted {
            reason: "the dial timed out".into(),
        }),
    }
}
