//! The Host and Origin gate every request passes before anything else looks at
//! it.
//!
//! Owned by the coordinator. It is the first thing `listener` consults and it runs
//! before any WebSocket upgrade, because the thing it refuses -- a DNS-rebinding
//! request -- is a request a browser can be made to send from any page it visits.
//!
//! Only engaged on a loopback bind (`apps/coord/src/middleware/coordinator-request-admission.ts:15`);
//! a coordinator bound to a routable interface has a front door doing this, and
//! doing it twice would admit less than either alone.
//!
//! FAILS CLOSED UNTIL THE PORT IS KNOWN. The gate's allowlist is built from the
//! RESOLVED port, and a request that arrives before the listener knows its own
//! port is answered `503 listener unavailable`
//! (`apps/coord/src/bun-coordinator-listeners.ts:363-365`) rather than allowed
//! through an empty allowlist.

/// What the gate decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// The request may proceed.
    Allowed,
    /// The `Host` header did not name a configured origin.
    ForbiddenHost,
    /// The `Origin` header was present and not allowed.
    ForbiddenOrigin,
    /// The listener does not yet know its own port.
    ListenerUnavailable,
}

impl Admission {
    /// The status this decision answers with.
    #[must_use]
    pub const fn status(self) -> u16 {
        match self {
            Admission::Allowed => 200,
            Admission::ForbiddenHost | Admission::ForbiddenOrigin => 403,
            Admission::ListenerUnavailable => 503,
        }
    }

    /// The plain-text body this decision answers with.
    ///
    /// `forbidden host` and `forbidden origin` are different strings on purpose:
    /// an operator debugging a reverse proxy needs to know which header was
    /// wrong, and neither string tells a prober anything it did not send.
    #[must_use]
    pub const fn body(self) -> &'static str {
        match self {
            Admission::Allowed => "",
            Admission::ForbiddenHost => "forbidden host",
            Admission::ForbiddenOrigin => "forbidden origin",
            Admission::ListenerUnavailable => "listener unavailable",
        }
    }
}

/// What the gate compares against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionPolicy {
    /// The listener's resolved `host:port`, or `None` before it is known.
    pub bound_port: Option<u16>,
    /// Bare origins allowed as a `Host` value.
    pub allowed_host_origins: Vec<String>,
    /// Bare origins allowed as an `Origin` value.
    pub allowed_origins: Vec<String>,
}

/// Decide one request.
///
/// A **missing** `Origin` is allowed: a non-browser client sends none, and the
/// credential -- not the origin -- is what authenticates it. A `Origin` that is
/// present and wrong is refused, and so is the literal `"null"`, which is what a
/// sandboxed frame and some privacy tools send.
#[must_use]
pub fn admit(host: Option<&str>, origin: Option<&str>, policy: &AdmissionPolicy) -> Admission {
    let Some(_port) = policy.bound_port else {
        return Admission::ListenerUnavailable;
    };

    if let Some(host) = host {
        let normalized = normalize_host_origins(host);
        if normalized
            .iter()
            .any(|candidate| policy.allowed_host_origins.contains(candidate))
        {
            // fall through to the origin check
        } else {
            return Admission::ForbiddenHost;
        }
    }

    match origin {
        None => Admission::Allowed,
        Some(origin)
            if policy
                .allowed_origins
                .iter()
                .any(|allowed| allowed == origin) =>
        {
            Admission::Allowed
        }
        Some(_) => Admission::ForbiddenOrigin,
    }
}

/// The bare authorities a `Host` header could mean, under both schemes.
///
/// Parsing under both `http://` and `https://` matters because the coordinator
/// never terminates TLS while an operator's front door usually does, and the same
/// `Host` reaches this gate either way
/// (`coordinator-request-admission.ts:59-73`). Everything except a bare
/// authority is discarded: userinfo, a path, a query and a fragment all mean the
/// header was not a `Host` header.
#[must_use]
pub fn normalize_host_origins(host: &str) -> Vec<String> {
    if host.is_empty() || host.trim() != host {
        return Vec::new();
    }
    if host.bytes().any(|byte| {
        byte <= 0x20 || byte == 0x7f || matches!(byte, b'\\' | b'/' | b'?' | b'#' | b'@')
    }) {
        return Vec::new();
    }
    // Both schemes, because the coordinator never terminates TLS while an
    // operator's front door usually does, and the same `Host` reaches this gate
    // either way. Parsed by hand rather than with a URL crate: the accepted
    // grammar is a bare authority and nothing else, and a general URL parser
    // would have to be *restricted* back down to that, which is a second place
    // to get it wrong.
    ["http", "https"]
        .into_iter()
        .map(|scheme| format!("{scheme}://{host}"))
        .collect()
}

/// The loopback authority for a resolved port, which is always allowed as a
/// `Host` on a loopback bind.
#[must_use]
pub fn loopback_authority(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}
