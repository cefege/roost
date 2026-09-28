//! The two WebSocket upgrade lifecycles: the worker's raw link and the
//! browser's Sync firehose.
//!
//! Owned by the coordinator's HTTP layer. Both handlers read the credential out
//! of `sec-websocket-protocol` and then apply a decision that belongs to
//! `worker_link::upgrade_admission` and `sync_ws::upgrade_admission`
//! respectively; this module is the transport around those decisions, and the
//! only two things it knows are the offered subprotocols and the two header
//! lookups both need.
//!
//! They are one module because they are one shape. The host/port decisions
//! above them are not here, and neither is the middleware stack in front of
//! them: a request that reaches either handler has already passed the admission
//! gate, so a Host or Origin rule restated here would be a second answer to a
//! question `http_admission` has already decided.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::response::{IntoResponse, Response};

use crate::http::listener::ListenerState;
use crate::worker_link::upgrade_admission::WORKER_WS_PATH_PREFIX;

/// The worker upgrade. The decision is in `worker_link::upgrade_admission`; this
/// only applies it.
pub async fn worker_upgrade(
    State(state): State<Arc<ListenerState>>,
    axum::extract::Path(fingerprint): axum::extract::Path<String>,
    request: Request,
) -> Response {
    let path = format!("{WORKER_WS_PATH_PREFIX}{fingerprint}");
    let offered = offered_protocols(&request);
    let query = request
        .uri()
        .query()
        .map_or_else(String::new, str::to_string);
    let upgrade = request.headers().get(axum::http::header::UPGRADE);

    // A request that is not an upgrade at all is an HTTP request to a WebSocket
    // path, and 400 says exactly that. Anything else would be a lie: the path
    // exists, the method does not.
    if upgrade.and_then(|value| value.to_str().ok()) != Some("websocket") {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; charset=utf-8",
            )],
            "upgrade required",
        )
            .into_response();
    }

    let credential = offered.get(1).cloned();
    let caller = match credential.as_deref() {
        Some(token) => crate::auth::authenticate::Authenticator {
            database: &state.services.db,
            keys: &state.services.jwt_keys,
            clock: crate::auth::jwt_verify::VerifyClock::at(crate::rpc::service::now_ms()),
            jwt_max_age_secs: state.service.config.jwt_max_age_secs,
        }
        .authenticate(token)
        .await
        .ok(),
        None => None,
    };
    let decision = crate::worker_link::upgrade_admission::admit_worker_upgrade(
        &crate::worker_link::upgrade_admission::WorkerUpgradeRequest {
            path,
            query,
            offered_protocols: offered,
            caller: caller.as_ref().map(|caller| {
                crate::worker_link::upgrade_admission::VerifiedWorkerCaller {
                    fingerprint: caller.fingerprint.clone(),
                    key_generation: caller.key_generation,
                    label: caller.label.clone(),
                }
            }),
        },
        caller
            .as_ref()
            .is_some_and(|caller| caller.principal.is_worker()),
        caller.as_ref().map(|caller| caller.key_generation),
    );
    match decision {
        crate::worker_link::upgrade_admission::UpgradeDecision::Refused(refusal) => (
            axum::http::StatusCode::from_u16(refusal.status())
                .unwrap_or(axum::http::StatusCode::UNAUTHORIZED),
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; charset=utf-8",
            )],
            refusal.body(),
        )
            .into_response(),
        decision @ crate::worker_link::upgrade_admission::UpgradeDecision::Admitted { .. } => {
            upgrade_worker_socket(request, decision, Arc::clone(&state.services)).await
        }
    }
}

/// The admitted half of the worker upgrade: switch protocols echoing ONLY the
/// marker, never the credential (`worker-ws-upgrade.ts:112-117`), and hand the
/// socket to `worker_link::connection::serve_socket`. A request the runtime
/// cannot upgrade answers `400 upgrade failed`, the contract's step 6.
async fn upgrade_worker_socket(
    request: Request,
    decision: crate::worker_link::upgrade_admission::UpgradeDecision,
    services: Arc<crate::services::CoordServices>,
) -> Response {
    use axum::extract::FromRequestParts as _;
    let (mut parts, _body) = request.into_parts();
    let upgrade =
        match axum::extract::ws::WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
            Ok(upgrade) => upgrade,
            Err(rejection) => {
                tracing::warn!(%rejection, "worker upgrade: the runtime upgrade failed");
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    [(
                        axum::http::header::CONTENT_TYPE,
                        "text/plain; charset=utf-8",
                    )],
                    "upgrade failed",
                )
                    .into_response();
            }
        };
    upgrade
        .protocols([crate::worker_link::upgrade_admission::WORKER_AUTH_SUBPROTOCOL])
        .max_message_size(crate::http::listener::MAX_WEBSOCKET_PAYLOAD_BYTES)
        .max_frame_size(crate::http::listener::MAX_WEBSOCKET_PAYLOAD_BYTES)
        .on_failed_upgrade(|error| {
            tracing::warn!(%error, "worker upgrade: the connection did not switch protocols");
        })
        .on_upgrade(move |socket| async move {
            crate::worker_link::connection::serve_socket(socket, decision, &services).await;
        })
}

/// The Sync upgrade. The decision is in `sync_ws::upgrade_admission`; an
/// admitted request is upgraded with the marker echoed, never the credential,
/// and handed to `sync_ws::socket::serve_socket` with no reauth deadline, as
/// v2's production listener passes none (`bun-coordinator-listeners.ts:328`).
pub async fn sync_upgrade(State(state): State<Arc<ListenerState>>, request: Request) -> Response {
    let offered = offered_protocols(&request);
    let credential = offered.get(1).cloned();
    let caller = match credential.as_deref() {
        Some(token) => crate::auth::authenticate::Authenticator {
            database: &state.services.db,
            keys: &state.services.jwt_keys,
            clock: crate::auth::jwt_verify::VerifyClock::at(crate::rpc::service::now_ms()),
            jwt_max_age_secs: state.service.config.jwt_max_age_secs,
        }
        .authenticate(token)
        .await
        .ok(),
        None => None,
    };
    let query = SyncQuery::of_uri(request.uri());
    let decision = crate::sync_ws::upgrade_admission::admit_sync_upgrade(
        &crate::sync_ws::upgrade_admission::SyncUpgradeRequest {
            path: request.uri().path().to_string(),
            origin: header_string(&request, axum::http::header::ORIGIN),
            host: header_string(&request, axum::http::header::HOST).unwrap_or_default(),
            offered_protocols: offered,
            caller: caller.as_ref().map(|caller| {
                crate::sync_ws::upgrade_admission::VerifiedSyncCaller {
                    fingerprint: caller.fingerprint.clone(),
                    label: caller.label.clone(),
                    key_generation: caller.key_generation,
                }
            }),
            tab: query.tab,
            since: query.since,
            flow: query.flow,
            sync_v: query.sync_v,
        },
        &crate::sync_ws::upgrade_admission::OriginPolicy {
            public_url: state.service.config.public_url.clone(),
            web_public_url: state.service.config.web_public_url.clone(),
            cors_allowed_origins: state.service.config.cors_allowed_origins.clone(),
            worker_local_ui_origin: roost_host::DEFAULT_WORKER_LOCAL_UI_ORIGIN.to_string(),
            loopback_bind: Some(state.service.config.bind.clone()),
            relaxed_csp: state.service.config.relaxed_csp,
        },
        match caller.as_ref().map(|caller| &caller.principal) {
            // An absent credential is already a refusal by the time this runs;
            // naming it a browser here would be a second, weaker answer.
            None => crate::sync_ws::upgrade_admission::PrincipalKind::AccountDevice,
            Some(crate::auth::principal::Principal::Worker { fingerprint, .. }) => {
                crate::sync_ws::upgrade_admission::PrincipalKind::Worker(fingerprint.clone())
            }
            // A pre-account key authenticates but has no scope to admit a
            // feed over: `404`, never folded into a browser.
            Some(crate::auth::principal::Principal::LegacySelfHosted { .. }) => {
                crate::sync_ws::upgrade_admission::PrincipalKind::LegacySelfHosted
            }
            Some(crate::auth::principal::Principal::AccountDevice { .. }) => {
                crate::sync_ws::upgrade_admission::PrincipalKind::AccountDevice
            }
        },
    );
    match decision {
        crate::sync_ws::upgrade_admission::SyncUpgradeDecision::Refused(refusal) => (
            axum::http::StatusCode::from_u16(refusal.status())
                .unwrap_or(axum::http::StatusCode::BAD_REQUEST),
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; charset=utf-8",
            )],
            refusal.body(),
        )
            .into_response(),
        crate::sync_ws::upgrade_admission::SyncUpgradeDecision::Admitted { caller, scope } => {
            upgrade_sync_socket(&state, request, caller, scope).await
        }
    }
}

/// Hijack an admitted Sync request: echo the marker and hand the socket to
/// the Sync loop. A request that is not a well-formed WebSocket handshake is
/// `400 upgrade failed`, v2's answer when `server.upgrade` refuses it
/// (`sync-ws-upgrade.ts:208-212`).
async fn upgrade_sync_socket(
    state: &Arc<ListenerState>,
    request: Request,
    caller: crate::sync_ws::upgrade_admission::VerifiedSyncCaller,
    scope: crate::sync_ws::upgrade_admission::SyncScope,
) -> Response {
    use axum::extract::FromRequestParts as _;
    let (mut parts, _body) = request.into_parts();
    let Ok(upgrade) =
        axum::extract::ws::WebSocketUpgrade::from_request_parts(&mut parts, state).await
    else {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; charset=utf-8",
            )],
            "upgrade failed",
        )
            .into_response();
    };
    let services = Arc::clone(&state.services);
    upgrade
        .protocols([crate::sync_ws::upgrade_admission::SYNC_AUTH_SUBPROTOCOL])
        .max_message_size(crate::http::listener::MAX_WEBSOCKET_PAYLOAD_BYTES)
        .on_upgrade(move |socket| {
            crate::sync_ws::socket::serve_socket(socket, caller, scope, None, services)
        })
}

/// The four query values a Sync upgrade reads, decoded as a browser's
/// `URLSearchParams.get` decodes them: percent-escapes and `+` resolved, and
/// the FIRST value when a name repeats (`sync-ws-upgrade.ts:172-180`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncQuery {
    /// `tab`, the browser tab this socket speaks for.
    pub tab: Option<String>,
    /// `since`, the last durable event id the client holds.
    pub since: Option<String>,
    /// `flow`, the ACK-window negotiation.
    pub flow: Option<String>,
    /// `sync_v`, the v2 negotiation.
    pub sync_v: Option<String>,
}

impl SyncQuery {
    /// Read the values out of a request's URI. A query that does not decode
    /// names nothing, which negotiates nothing: the socket is a plain v1 feed,
    /// exactly as it would be with no query at all.
    #[must_use]
    pub fn of_uri(uri: &axum::http::Uri) -> Self {
        let pairs = axum::extract::Query::<Vec<(String, String)>>::try_from_uri(uri)
            .map(|axum::extract::Query(pairs)| pairs)
            .unwrap_or_default();
        let first = |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        };
        Self {
            tab: first("tab"),
            since: first("since"),
            flow: first("flow"),
            sync_v: first("sync_v"),
        }
    }
}

/// The subprotocols this request offers, in the order it offered them.
///
/// The credential is the SECOND entry by contract, and the order matters: a
/// client that offers one subprotocol is offering a marker, not a credential,
/// and reading position zero as one would authenticate nothing.
fn offered_protocols(request: &Request) -> Vec<String> {
    request
        .headers()
        .get("sec-websocket-protocol")
        .and_then(|value| value.to_str().ok())
        .map(|raw| raw.split(',').map(str::trim).map(str::to_string).collect())
        .unwrap_or_default()
}

fn header_string(request: &Request, name: axum::http::HeaderName) -> Option<String> {
    request
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}
