//! The loopback door's one request handler (v2 `local-door/local-ui-server.ts`
//! `fetch`): the Host and Origin gate, the bootstrap, the two socket upgrades,
//! and the page fallback, every answer stamped with the shared security
//! headers. Mounted by `runtime::door_serve` on the bound listener; reads its
//! policy from `crate::door` and hands upgraded sockets to the route owners.

use std::fmt::Display;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{FromRequestParts as _, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use tokio::sync::watch;

use crate::door::admission::DoorAdmission;
use crate::door::loopback::{LoopbackOwner, LoopbackRoutes};
use crate::door::spa::SpaMount;
use crate::door::{
    LOCAL_ATTACHMENT_PATH, LOCAL_BOOTSTRAP_PATH, LOCAL_TERMINAL_MAX_PAYLOAD_BYTES,
    LOCAL_TERMINAL_PATH,
};

/// Everything one serving door answers from, fixed when it starts serving.
#[derive(Debug)]
pub struct DoorState {
    pub admission: DoorAdmission,
    /// v2 `applySecurityHeaders(headers, false, false, connectOrigins)`,
    /// resolved once: the values cannot change while the door serves.
    pub security: Vec<(HeaderName, HeaderValue)>,
    /// `{"coordinatorUrl":…,"workerFingerprint":…}`, serialized once.
    pub bootstrap_body: Bytes,
    pub spa: SpaMount,
    pub sockets: LoopbackRoutes,
    /// Flips when the door stops; every socket pump watches it.
    pub stop: watch::Receiver<bool>,
}

/// The door's router: one fallback, because v2 branches on the path inside a
/// single handler and a path a route table does not name is still a page.
pub fn router(state: Arc<DoorState>) -> Router {
    Router::new().fallback(fetch).with_state(state)
}

/// What a refusal log line names, read before the request is consumed.
struct RequestFacts {
    method: Method,
    path: String,
    host: Option<String>,
    origin: Option<String>,
}

impl RequestFacts {
    fn of(request: &Request) -> Self {
        let header = |name: header::HeaderName| {
            request
                .headers()
                .get(name)
                .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned())
        };
        Self {
            method: request.method().clone(),
            path: request.uri().path().to_owned(),
            host: header(header::HOST),
            origin: header(header::ORIGIN),
        }
    }
}

async fn fetch(State(door): State<Arc<DoorState>>, request: Request) -> Response {
    let facts = RequestFacts::of(&request);
    let response = if !door
        .admission
        .admits_host(facts.host.as_deref().unwrap_or_default())
    {
        refused(StatusCode::FORBIDDEN, "host_not_local", &facts)
    } else if facts
        .origin
        .as_deref()
        .is_some_and(|origin| !door.admission.admits_origin(origin))
    {
        refused(StatusCode::FORBIDDEN, "origin_not_local", &facts)
    } else if facts.path == LOCAL_BOOTSTRAP_PATH {
        bootstrap(&door, &facts)
    } else if facts.path == LOCAL_TERMINAL_PATH {
        upgrade(&door, request, &facts, &door.sockets.terminal).await
    } else if facts.path == LOCAL_ATTACHMENT_PATH {
        match &door.sockets.attachment {
            Some(owner) => upgrade(&door, request, &facts, owner).await,
            None => refused(StatusCode::NOT_FOUND, "attachment_unavailable", &facts),
        }
    } else if !matches!(facts.method, Method::GET | Method::HEAD) {
        refused(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed", &facts)
    } else {
        let accept_encoding = request
            .headers()
            .get(header::ACCEPT_ENCODING)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        door.spa
            .respond(&facts.path, &facts.method, accept_encoding)
            .await
    };
    secured(&door, response)
}

/// Where this machine's coordinator is and which worker this is, for a page
/// that has nothing else yet.
fn bootstrap(door: &DoorState, facts: &RequestFacts) -> Response {
    let cors = door.admission.cors_origin(facts.origin.as_deref());
    match facts.method {
        // Chrome's local-network-access check may preflight a public→loopback
        // GET, and a 405 there fails the probe with nothing to diagnose.
        Method::OPTIONS => {
            let mut headers = HeaderMap::new();
            if let Some(origin) = cors {
                allow_origin(&mut headers, origin);
                headers.insert(
                    header::ACCESS_CONTROL_ALLOW_METHODS,
                    HeaderValue::from_static("GET"),
                );
                headers.insert(
                    "access-control-allow-private-network",
                    HeaderValue::from_static("true"),
                );
                headers.insert(
                    header::ACCESS_CONTROL_MAX_AGE,
                    HeaderValue::from_static("600"),
                );
            }
            (StatusCode::NO_CONTENT, headers).into_response()
        }
        Method::GET | Method::HEAD => {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            // The advertised coordinator follows the worker's own config; a
            // cached copy would outlive a redeploy that moved it.
            if let Some(origin) = cors {
                allow_origin(&mut headers, origin);
            }
            let body = if facts.method == Method::HEAD {
                Bytes::new()
            } else {
                door.bootstrap_body.clone()
            };
            (StatusCode::OK, headers, Body::from(body)).into_response()
        }
        _ => refused(StatusCode::METHOD_NOT_ALLOWED, "bootstrap_method", facts),
    }
}

/// A page on the coordinator's origin reads the bootstrap cross-origin, so the
/// allowance rides on the response. No credentials, so no allow-credentials.
fn allow_origin(headers: &mut HeaderMap, origin: &str) {
    if let Ok(value) = HeaderValue::from_str(origin) {
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, value);
        headers.insert(header::VARY, HeaderValue::from_static("origin"));
    }
}

/// Upgrade a loopback socket onto its route's owner, in v2's order: method,
/// then subprotocol, then the handshake itself.
async fn upgrade(
    door: &DoorState,
    request: Request,
    facts: &RequestFacts,
    owner: &LoopbackOwner,
) -> Response {
    let route = owner.route();
    if facts.method != Method::GET {
        return refused(
            StatusCode::METHOD_NOT_ALLOWED,
            format_args!("{}_method", route.name()),
            facts,
        );
    }
    let offered = request
        .headers()
        .get_all(header::SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .flat_map(|value| value.as_bytes().split(|byte| *byte == b','))
        .any(|protocol| protocol.trim_ascii() == route.subprotocol().as_bytes());
    if !offered {
        return refused(
            StatusCode::BAD_REQUEST,
            format_args!("{}_subprotocol", route.name()),
            facts,
        );
    }
    let (mut parts, _body) = request.into_parts();
    let Ok(handshake) = WebSocketUpgrade::from_request_parts(&mut parts, &()).await else {
        return refused(
            StatusCode::BAD_REQUEST,
            format_args!("{}_not_upgradable", route.name()),
            facts,
        );
    };
    let socket_id = match crate::session::ids::mint_uuid() {
        Ok(socket_id) => socket_id,
        Err(error) => {
            tracing::error!(%error, route = route.name(), "a local door socket could not be given an id");
            return refused(
                StatusCode::INTERNAL_SERVER_ERROR,
                format_args!("{}_socket_id", route.name()),
                facts,
            );
        }
    };
    let owner = owner.clone();
    let stop = door.stop.clone();
    // One protobuf envelope per frame and no per-message compression: the hop
    // is loopback, so deflate would only add an allocation.
    handshake
        .protocols([route.subprotocol()])
        .max_message_size(LOCAL_TERMINAL_MAX_PAYLOAD_BYTES)
        .max_frame_size(LOCAL_TERMINAL_MAX_PAYLOAD_BYTES)
        .on_upgrade(move |socket| owner.serve_socket(socket_id, socket, stop))
}

/// A refusal: logged with what the request claimed, answered with no body.
fn refused(status: StatusCode, reason: impl Display, facts: &RequestFacts) -> Response {
    tracing::warn!(
        %reason,
        status = status.as_u16(),
        method = %facts.method,
        path = %facts.path,
        host = ?facts.host,
        origin = ?facts.origin,
        "local_ui_rejected"
    );
    status.into_response()
}

/// Every answer carries the security headers except the handshake that
/// switches protocols, which v2 answered with the subprotocol alone.
fn secured(door: &DoorState, mut response: Response) -> Response {
    if response.status() == StatusCode::SWITCHING_PROTOCOLS {
        let headers = response.headers_mut();
        for (name, value) in &door.security {
            headers.insert(name.clone(), value.clone());
        }
    }
    response
}
