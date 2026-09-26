//! The Host/Origin admission layer, mounted in front of every route and
//! therefore in front of both WebSocket upgrades.
//!
//! Owned by the middleware stack. `http::listener` mounts [`admission_gate`]
//! as the outermost layer; the DECISIONS live in [`crate::http_admission`],
//! which this module applies and does not restate. The layer's own job is the
//! part a pure decision function cannot have: knowing whether the listener has
//! yet learned the port its own allowlist is built from.
//!
//! FAILS CLOSED UNTIL THE PORT IS KNOWN. The allowlist names the RESOLVED
//! loopback authority, and `bind: 127.0.0.1:0` only resolves after the
//! listener binds. A gate that answered from an empty allowlist in that window
//! would refuse every request -- a boot-time outage -- so the pre-bind answer is
//! `503 listener unavailable`, which tells a caller to retry rather than that
//! it is forbidden
//! (`apps/coord/src/bun-coordinator-listeners.ts:279-282,368-370`).
//!
//! THE LAYER COVERS THE WHOLE SURFACE, NOT JUST THE SOCKETS. v2 runs the gate
//! first thing in the fetch handler -- before the worker upgrade, before the
//! Sync upgrade, and before Connect -- so mounting it around the router rather
//! than around the two upgrade routes reproduces that. Scoping it to the
//! upgrades would leave Connect answering a DNS-rebinding request that the
//! sockets refuse, which is the same bypass with a different path.

use std::sync::OnceLock;

use axum::http::{HeaderName, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::http_admission::{Admission, AdmissionPolicy, admit, loopback_authority};

/// The allowlist inputs that do not depend on the resolved port.
#[derive(Debug)]
struct AdmissionSources {
    /// Origins allowed to send a cross-origin request: the worker's loopback
    /// door and the operator's own entries.
    cross_origin: Vec<String>,
    /// The operator's declared front doors, which are allowed as a `Host` as
    /// well as an `Origin` because a request through one arrives bearing that
    /// door's own name.
    declared_front_doors: Vec<String>,
}

/// The Host/Origin gate, with the port it is built from published once.
#[derive(Debug)]
pub struct AdmissionLayer {
    /// A coordinator bound to a routable interface has a front door doing this
    /// gate, and doing it twice admits less than either alone
    /// (`coordinator-request-admission.ts:15`).
    engaged: bool,
    sources: AdmissionSources,
    /// Resolved once, when the listener publishes the port it bound. Runtime
    /// input rather than a constant, so this is a cell and not a lazy static.
    policy: OnceLock<AdmissionPolicy>,
}

impl AdmissionLayer {
    /// The gate a coordinator with this configuration starts with.
    ///
    /// Every request is refused with `503 listener unavailable` until
    /// [`Self::publish_bound_port`] is called.
    #[must_use]
    pub fn from_config(config: &roost_host::CoordConfig) -> Self {
        let mut cross_origin = vec![roost_host::DEFAULT_WORKER_LOCAL_UI_ORIGIN.to_owned()];
        cross_origin.extend(config.cors_allowed_origins.iter().cloned());
        let declared_front_doors = [config.web_public_url.clone(), config.public_url.clone()]
            .into_iter()
            .flatten()
            .filter(|declared| !declared.is_empty())
            .collect();
        Self {
            engaged: is_loopback_coordinator_bind(&config.bind),
            sources: AdmissionSources {
                cross_origin,
                declared_front_doors,
            },
            policy: OnceLock::new(),
        }
    }

    /// Whether this gate refuses anything at all.
    #[must_use]
    pub fn is_engaged(&self) -> bool {
        self.engaged
    }

    /// Tell the gate which port the listener actually bound.
    ///
    /// Refuses a second publish rather than overwriting: a process binds one
    /// listener, so a second call is a defect, and silently moving the
    /// allowlist would re-open a gate that has already admitted requests under
    /// the port it was built for.
    pub fn publish_bound_port(&self, port: u16) {
        if !self.engaged {
            tracing::debug!(
                "coordinator admission gate is not engaged on this bind; nothing to publish"
            );
            return;
        }
        let local = loopback_authority(port);
        let policy = AdmissionPolicy {
            bound_port: Some(port),
            allowed_host_origins: std::iter::once(local.clone())
                .chain(self.sources.declared_front_doors.iter().cloned())
                .collect(),
            allowed_origins: std::iter::once(local)
                .chain(self.sources.cross_origin.iter().cloned())
                .chain(self.sources.declared_front_doors.iter().cloned())
                .collect(),
        };
        if self.policy.set(policy).is_err() {
            tracing::warn!(
                target: "coord-admission",
                "a second bound port was refused; the gate keeps the port it was built for"
            );
            return;
        }
        tracing::info!(
            target: "coord-admission",
            port,
            "coordinator admission gate published its bound port"
        );
    }
}

/// Whether a bind is the loopback form the gate's allowlist is built for.
///
/// The spelling is exact rather than "any loopback address": the allowlist is
/// the one canonical loopback authority for the port the listener bound, and a
/// `localhost` or `::1` bind resolves to a port this gate would name wrongly.
fn is_loopback_coordinator_bind(bind: &str) -> bool {
    let Some(port) = bind.strip_prefix("127.0.0.1:") else {
        return false;
    };
    !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit())
}

/// The response one admission decision answers with.
///
/// The body names the header that was wrong, because an operator debugging a
/// reverse proxy needs to know that, and neither body tells a prober anything it
/// did not send.
fn refusal_response(decision: Admission) -> Response {
    if matches!(decision, Admission::ListenerUnavailable) {
        tracing::debug!(
            target: "coord-admission",
            "a request arrived before the listener published its port"
        );
    } else {
        tracing::warn!(
            target: "coord-admission",
            status = decision.status(),
            reason = decision.body(),
            "request_rejected"
        );
    }
    let status = StatusCode::from_u16(decision.status()).unwrap_or(StatusCode::FORBIDDEN);
    (
        status,
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; charset=utf-8",
        )],
        decision.body(),
    )
        .into_response()
}

/// An `Origin` header as the gate has to read it.
enum OriginHeader {
    /// The client sent none, which a non-browser client does.
    Absent,
    /// The client sent one the gate can compare.
    Value(String),
    /// The client sent bytes that are not an origin this gate can read. They
    /// are refused rather than treated as absent: a header a gate cannot read
    /// is not evidence that no header was sent.
    Unreadable,
}

/// The `Origin` header as the gate compares it.
fn origin_header(request: &Request) -> OriginHeader {
    match request.headers().get(HeaderName::from_static("origin")) {
        None => OriginHeader::Absent,
        Some(value) => match value.to_str() {
            Ok(origin) => OriginHeader::Value(origin.to_owned()),
            Err(_) => OriginHeader::Unreadable,
        },
    }
}

/// The `Host` header as the gate compares it.
///
/// A `Host` this gate cannot read is passed on as bytes it cannot match, which
/// refuses it; it is never dropped, because an unreadable header is not
/// evidence that no header was sent.
fn host_header(request: &Request) -> Option<String> {
    request
        .headers()
        .get(HeaderName::from_static("host"))
        .map(|value| {
            value.to_str().map_or_else(
                |_| String::from_utf8_lossy(value.as_bytes()).into_owned(),
                str::to_owned,
            )
        })
}

/// Refuse what the admission policy refuses, and pass on what it allows.
pub async fn admission_gate(
    State(gate): State<std::sync::Arc<AdmissionLayer>>,
    request: Request,
    next: Next,
) -> Response {
    if !gate.engaged {
        return next.run(request).await;
    }
    // The pre-bind answer is `admit`'s own answer for a policy with no port,
    // reached through the same refusal response as any other decision, so there
    // is exactly one way this gate says no.
    let Some(policy) = gate.policy.get() else {
        return refusal_response(Admission::ListenerUnavailable);
    };
    let host = host_header(&request);
    let origin = origin_header(&request);
    let decision = match &origin {
        OriginHeader::Unreadable => Admission::ForbiddenOrigin,
        OriginHeader::Absent => admit(host.as_deref(), None, policy),
        OriginHeader::Value(origin) => admit(host.as_deref(), Some(origin), policy),
    };
    match decision {
        Admission::Allowed => next.run(request).await,
        refused => refusal_response(refused),
    }
}

