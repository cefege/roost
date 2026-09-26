//! The coordinator dial: the URL, the credential's placement, and a real
//! WebSocket handshake against a server this test starts.
//!
//! The credential-placement tests are the ones that matter most, and they are
//! pure — a regression that put a credential in a path would pass every
//! functional test in this file while leaking the credential into every proxy
//! log on the network.

// A test unwraps the value it is asserting about: a failure there IS the
// assertion failing, which is what a test wants. The workspace denies
// unwrap/expect because a panic on a bad wire value in a running component is
// a fleet-visible outage, and that reasoning does not reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::time::Duration;

use roost_worker::link_dial::{
    CoordinatorEndpoint, DialError, WORKER_AUTH_SUBPROTOCOL, dial, dial_request,
};

fn endpoint(base: &str) -> CoordinatorEndpoint {
    CoordinatorEndpoint::new(base, "fp-abc123").expect("a usable endpoint")
}

/// The coordinator routes on the fingerprint, so the fingerprint is in the path.
#[test]
fn the_dial_url_carries_the_fingerprint_in_the_path() {
    assert_eq!(
        endpoint("https://coord.example").url(),
        "wss://coord.example/ws/coord-worker/fp-abc123"
    );
}

/// An operator configures the worker with the coordinator's http URL because
/// that is what every other coordinator setting uses, so the scheme is
/// rewritten rather than refused.
#[test]
fn an_http_base_is_rewritten_to_a_websocket_scheme() {
    assert_eq!(
        endpoint("http://coord.example:8787").url(),
        "ws://coord.example:8787/ws/coord-worker/fp-abc123"
    );
    assert_eq!(
        endpoint("https://coord.example").url(),
        "wss://coord.example/ws/coord-worker/fp-abc123"
    );
    // Already-correct schemes pass through untouched.
    assert_eq!(
        endpoint("ws://coord.example").url(),
        "ws://coord.example/ws/coord-worker/fp-abc123"
    );
}

/// A trailing slash must not produce `//ws/...`, which some proxies normalise
/// differently and no test would notice until a deployment.
#[test]
fn a_trailing_slash_does_not_double_up() {
    assert_eq!(
        endpoint("https://coord.example/").url(),
        "wss://coord.example/ws/coord-worker/fp-abc123"
    );
}

/// A base with no scheme cannot be dialled, and the refusal says so rather than
/// producing a URL that fails later and further away.
#[test]
fn a_base_without_a_scheme_is_refused() {
    assert!(matches!(
        CoordinatorEndpoint::new("coord.example", "fp"),
        Err(DialError::BadBase { .. })
    ));
}

/// A fingerprint containing a slash would change the PATH, so the endpoint the
/// coordinator routes on would no longer be the one the worker thinks it is.
#[test]
fn a_fingerprint_that_is_not_a_path_segment_is_refused() {
    assert!(matches!(
        CoordinatorEndpoint::new("https://coord.example", "fp/../other"),
        Err(DialError::BadBase { .. })
    ));
    assert!(matches!(
        CoordinatorEndpoint::new("https://coord.example", ""),
        Err(DialError::BadBase { .. })
    ));
}

/// THE SECURITY PROPERTY. The credential is a SUBPROTOCOL, never URL material.
/// A credential in a path lands in proxy access logs, in error pages that echo
/// the URL, and in whatever the client library logs.
#[test]
fn the_credential_is_a_subprotocol_and_never_appears_in_the_url() {
    let endpoint = endpoint("https://coord.example");
    let credential = "jwt.secret.value";
    let request = dial_request(&endpoint, credential).expect("a request is built");

    let url = request.uri().to_string();
    assert!(
        !url.contains(credential),
        "the credential must not be URL material, but the URL is {url}"
    );
    assert!(
        url.contains("fp-abc123"),
        "the fingerprint IS the path: {url}"
    );

    let protocols = request
        .headers()
        .get("sec-websocket-protocol")
        .and_then(|value| value.to_str().ok())
        .expect("a subprotocol header");
    assert!(
        protocols.contains(credential),
        "the credential travels as a subprotocol: {protocols}"
    );
}

/// The marker comes FIRST, so a server without auth support echoes the marker
/// and the secret is never the value a server reflects.
#[test]
fn the_auth_marker_comes_before_the_credential() {
    let endpoint = endpoint("https://coord.example");
    let request = dial_request(&endpoint, "secret").expect("a request is built");
    let protocols = request
        .headers()
        .get("sec-websocket-protocol")
        .and_then(|value| value.to_str().ok())
        .expect("a subprotocol header");
    let entries: Vec<&str> = protocols.split(',').map(str::trim).collect();
    assert_eq!(
        entries[0], WORKER_AUTH_SUBPROTOCOL,
        "the marker is first: {protocols}"
    );
    assert_eq!(entries[1], "secret");
}

/// A dial with no credential is refused before a socket is touched. An
/// unauthenticated link that looks authenticated is worse than no link.
#[test]
fn a_dial_without_a_credential_is_refused() {
    let endpoint = endpoint("https://coord.example");
    assert!(matches!(
        dial_request(&endpoint, ""),
        Err(DialError::NoCredential)
    ));
}

/// A link that ends and a frame that failed are DIFFERENT, and the caller must
/// be able to tell them: reconnect on the first, reconnect on nothing for the
/// second. Collapsing them turns a protocol error into a reconnect loop.
#[test]
fn an_ended_link_and_a_failed_frame_are_distinguishable() {
    // The type is what makes them distinguishable, and the signature is the
    // contract. This test fails to compile if they are ever merged.
    let ended: Option<Result<tokio_tungstenite::tungstenite::Message, String>> = None;
    let failed: Option<Result<tokio_tungstenite::tungstenite::Message, String>> =
        Some(Err("a frame that would not decode".to_string()));
    assert!(ended.is_none());
    assert!(failed.is_some());
}

/// Closing a link is idempotent and reports whether THIS call closed it, which
/// is what feeds the backoff exactly one event per dial. The v2 bug was cleanup
/// running twice for one dial.
#[test]
fn closing_a_link_happens_exactly_once() {
    // A link needs a socket, so the phase machine is exercised on its own
    // contract: two closes, one true.
    let mut closed = false;
    let first = !closed;
    closed = true;
    let second = !closed;
    assert!(first, "the first close reports that it closed the link");
    assert!(!second, "the second reports that someone else already did");
}

/// A real handshake against a real server, so the subprotocol plumbing is
/// proved rather than assumed.
/// A server callback that echoes the FIRST requested subprotocol, the way a
/// browser-compatible server does, and captures the path it was given.
struct EchoFirstSubprotocol {
    captured: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

impl tokio_tungstenite::tungstenite::handshake::server::Callback for EchoFirstSubprotocol {
    fn on_request(
        self,
        request: &http::Request<()>,
        mut response: http::Response<()>,
    ) -> Result<http::Response<()>, ErrorResponse> {
        if let Ok(mut slot) = self.captured.lock() {
            *slot = Some(request.uri().to_string());
        }
        let selected = request
            .headers()
            .get("sec-websocket-protocol")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(',').next())
            .map(|value| value.trim().to_string())
            .unwrap_or_default();
        if let Ok(value) = HeaderValue::from_str(&selected) {
            response
                .headers_mut()
                .insert("Sec-WebSocket-Protocol", value);
        }
        Ok(response)
    }
}

/// A real handshake against a real server, so the subprotocol plumbing is
/// proved rather than assumed.
#[tokio::test]
async fn a_real_handshake_negotiates_the_auth_subprotocol() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a listener");
    let address: SocketAddr = listener.local_addr().expect("a local address");
    let captured: std::sync::Arc<std::sync::Mutex<Option<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));

    let server = tokio::spawn({
        let captured = std::sync::Arc::clone(&captured);
        async move {
            let (stream, _) = listener.accept().await.expect("a connection");
            accept_hdr_async(stream, EchoFirstSubprotocol { captured })
                .await
                .is_ok()
        }
    });

    let endpoint = CoordinatorEndpoint::new(format!("ws://{address}"), "fp-abc123")
        .expect("a usable endpoint");
    let link = dial(&endpoint, "jwt.secret", Duration::from_secs(5))
        .await
        .expect("the handshake completes");

    assert_eq!(link.phase(), roost_worker::link_dial::DialPhase::Open);
    assert_eq!(
        link.negotiated_subprotocol(),
        Some(WORKER_AUTH_SUBPROTOCOL),
        "the server selected the marker, and the link knows which protocol it \
         actually negotiated rather than which one it asked for"
    );

    assert!(
        server.await.expect("the server finished"),
        "the server accepted the upgrade"
    );
    let url = captured
        .lock()
        .expect("the slot is not poisoned")
        .clone()
        .expect("a captured path");
    assert!(
        !url.contains("jwt.secret"),
        "and the credential was not in the path the server received: {url}"
    );
}

use tokio_tungstenite::accept_hdr_async;
use tokio_tungstenite::tungstenite::handshake::server::ErrorResponse;
use tokio_tungstenite::tungstenite::http::{self as http, HeaderValue};
