//! The caller's origin profile: what the transport under a request proves about
//! who is calling, and which routes demand a caller that arrived on this host.
//!
//! Owned by the middleware stack. `http::listener` mounts
//! [`caller_origin_layer`] immediately below the admission gate, so every
//! request the gate admits carries a resolved [`CallerOrigin`] for the rate
//! limiter, the audit hook, and the on-host routes. It reads
//! `coord_core::ListenerTrust` and `CoordConfig`; it decides nothing either of
//! them does not already say.
//!
//! THE TRUST PROFILE IS CHOSEN AT BOOT AND NEVER SNIFFED
//! (`apps/coord/src/middleware/caller-origin.ts:8-10`). A profile read out of a
//! request header is a profile the client wrote, and the profile decides
//! whether a remote caller may claim to be on the host -- so a header that
//! turned `direct` into `trusted-proxy` would hand every remote caller the
//! address of the proxy.

use std::net::SocketAddr;

use axum::extract::{Request, State};
use axum::http::{Extensions, HeaderMap, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;
use roost_protocol::wire::headers::{X_ROOST_ON_HOST, X_ROOST_REMOTE_ADDR};

use crate::coord_core::ListenerTrust;

/// The body every on-host refusal answers with, in the operator's words.
pub const ON_HOST_ONLY: &str = "on-host only";

/// The address a listener with no observed peer reports.
///
/// Not a routable placeholder: a rate-limiter bucket keyed on it is shared by
/// every request whose peer the listener could not name, which is the correct
/// outcome -- an unnameable caller spends one budget, not one budget each.
pub const UNKNOWN_PEER: &str = "unknown";

/// The peer addresses that are this host, as a listener spells them.
///
/// The three spellings are the three an IPv4-mapped IPv6 peer can arrive in
/// (`caller-origin.ts:22-26`). A peer address is a bare IP: the port is not
/// part of the identity, and keying a bucket on it would hand every ephemeral
/// port its own budget.
#[must_use]
pub fn is_loopback_peer(address: &str) -> bool {
    matches!(address, "127.0.0.1" | "::1" | "::ffff:127.0.0.1")
}

/// The trust profile a listener with this `trust_proxy` setting boots with.
#[must_use]
pub const fn listener_trust(trust_proxy: bool) -> ListenerTrust {
    if trust_proxy {
        ListenerTrust::Forwarded
    } else {
        ListenerTrust::DirectLoopback
    }
}

/// Who is calling, as far as the transport underneath the request proves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallerOrigin {
    /// The boot-selected trust profile of the listener that accepted this
    /// request.
    pub listener: ListenerTrust,
    /// The real client address, for rate limiting and audit.
    pub client_ip: String,
    /// Whether the request reached this coordinator on its own host and
    /// traversed no proxy. This gates the most sensitive endpoints, so it is
    /// `false` whenever there is any doubt at all.
    pub on_host: bool,
}

impl CallerOrigin {
    /// A profile for a caller the listener observed itself.
    #[must_use]
    pub fn local(address: &str) -> Self {
        Self {
            listener: ListenerTrust::DirectLoopback,
            client_ip: address.to_owned(),
            on_host: is_loopback_peer(address),
        }
    }

    /// A profile for a caller with no observed address, which is never local.
    #[must_use]
    pub fn unknown() -> Self {
        Self {
            listener: ListenerTrust::DirectLoopback,
            client_ip: UNKNOWN_PEER.to_owned(),
            on_host: false,
        }
    }
}

/// Resolve one request's caller origin.
///
/// The rule that matters is the second one: under a trusted proxy, the mere
/// PRESENCE of `X-Forwarded-For` proves the request traversed that proxy, which
/// is what disqualifies it from on-host authority. A proxied request from a
/// browser on the coordinator's own host still fails this, which is the point --
/// the front door is a different trust domain from the host it fronts.
///
/// A forwarded header that is present but blank asserts no address, so it
/// supplies none -- but its PRESENCE still proves a proxy was traversed, which
/// is a different question and is answered from the header's existence.
#[must_use]
pub fn resolve_caller_origin(
    trust: ListenerTrust,
    socket_peer: Option<&str>,
    forwarded_for: Option<&str>,
) -> CallerOrigin {
    if !trust.asserts_locality() {
        return CallerOrigin {
            listener: trust,
            client_ip: first_forwarded_address(forwarded_for)
                .or_else(|| socket_peer.map(str::to_owned))
                .unwrap_or_else(|| UNKNOWN_PEER.to_owned()),
            on_host: forwarded_for.is_none() && socket_peer.is_some_and(is_loopback_peer),
        };
    }
    match socket_peer {
        Some(address) => CallerOrigin::local(address),
        None => CallerOrigin::unknown(),
    }
}

/// The first address a forwarding proxy asserts.
///
/// Only the first entry is the caller: the rest is the chain the proxy itself
/// learned, which is exactly the part a client can write.
fn first_forwarded_address(forwarded_for: Option<&str>) -> Option<String> {
    let first = forwarded_for?.split(',').next().unwrap_or_default().trim();
    (!first.is_empty()).then(|| first.to_owned())
}

/// The profile the caller-origin layer resolved for this request, if it ran.
///
/// A `None` here is a wiring fault -- every request that reached a handler went
/// through the layer -- so a caller that reads this treats it as "not on host"
/// rather than as an anonymous local operator.
#[must_use]
pub fn from_extensions(extensions: &Extensions) -> Option<&CallerOrigin> {
    extensions.get::<CallerOrigin>()
}

/// The peer address the listener observed, as a bare IP.
///
/// The port is dropped because a rate-limit budget belongs to a client, and a
/// client that reconnects on a new ephemeral port is the same client.
fn observed_peer(request: &Request) -> Option<String> {
    request
        .extensions()
        .get::<axum::extract::ConnectInfo<SocketAddr>>()
        .map(|peer| peer.0.ip().to_string())
}

/// The raw `X-Forwarded-For` value, whose mere presence is the evidence that
/// this request traversed a proxy.
fn forwarded_for(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// Stamp the two wire-contract headers a front door's log and the browser's
/// interceptor can read (`rpc/bun-handler.ts:61-62`).
///
/// The listener-trust header is deliberately NOT stamped: its contract value is
/// the `1` sentinel of the browser-to-coordinator direction
/// (`roost_protocol::wire::headers::LISTENER_TRUST_YES`), and writing a
/// `"direct"`/`"trusted-proxy"` spelling here would create a second value for
/// the one fact the boot profile already answers.
fn stamp_wire_headers(headers: &mut HeaderMap, origin: &CallerOrigin) {
    if let Ok(address) = HeaderValue::from_str(&origin.client_ip) {
        headers.insert(X_ROOST_REMOTE_ADDR, address);
    }
    headers.insert(
        X_ROOST_ON_HOST,
        if origin.on_host {
            HeaderValue::from_static("1")
        } else {
            HeaderValue::from_static("0")
        },
    );
}

/// Resolve the caller origin once per request and hand it to everything below.
///
/// This is the single owner of the on-host answer for the whole listener: the
/// Connect auth gate, the rate limiter, the audit hook and the export route all
/// read the extension this inserts, so a route cannot answer "is this local?"
/// with a weaker rule than its neighbour.
pub async fn caller_origin_layer(
    State(trust): State<ListenerTrust>,
    mut request: Request,
    next: Next,
) -> Response {
    let peer = observed_peer(&request);
    let forwarded = forwarded_for(request.headers());
    let origin = resolve_caller_origin(trust, peer.as_deref(), forwarded.as_deref());
    stamp_wire_headers(request.headers_mut(), &origin);
    request.extensions_mut().insert(origin);
    next.run(request).await
}
