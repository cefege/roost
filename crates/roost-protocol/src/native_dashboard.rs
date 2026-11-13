//! The one wire shape behind a hosted OMP stats dashboard.
//!
//! The native dashboard listens on a worker's loopback. A browser reaches it
//! through a coordinator capability, never by dialling the worker's address, so
//! the capability route names the upstream target and this module owns every
//! rule that target, a request, and a response must satisfy.
//!
//! The rules are shared by the coordinator route and the worker proxy: one
//! extraction of the target from the native `/stats` URL, one path check, one
//! header allowlist. A second copy on either side would be the defect this
//! crate exists to prevent -- a rule the two ends disagree about.
//!
//! No value here is persisted: a target lives only in coordinator memory for
//! the capability's lifetime.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};

mod resource;
pub use resource::{DashboardOpen, ResourceKey, StreamRequest, StreamResponse, fragment_is_safe};

/// The coordinator path namespace a capability URL lives under.
pub const CAPABILITY_PREFIX: &str = "/omp-stats";

/// One upstream request body. The native dashboard's largest POST carries no
/// body; the ceiling is generous enough for a future action and small enough
/// that a worker never buffers a file.
pub const MAX_REQUEST_BODY_BYTES: usize = 1024 * 1024;

/// One upstream response body. Static dashboard assets fit; a trace page does
/// not, and the caller is told rather than served half a document.
pub const MAX_RESPONSE_BODY_BYTES: usize = 2 * 1024 * 1024;

/// Request headers a capability may forward. Everything else -- cookies,
/// authorization, a caller's host -- is dropped rather than relayed.
pub const ALLOWED_REQUEST_HEADERS: &[&str] = &[
    "accept",
    "cache-control",
    "content-type",
    "if-none-match",
    "last-event-id",
    "pragma",
    "x-omp-stats-action",
];

/// Response headers a capability may hand back. `set-cookie` and `location`
/// are absent by construction: a native response may not plant a browser
/// credential or move the page to another origin.
pub const ALLOWED_RESPONSE_HEADERS: &[&str] = &[
    "cache-control",
    "content-language",
    "content-type",
    "etag",
    "expires",
    "last-modified",
];

pub const MAX_HEADERS: usize = 16;
pub const MAX_HEADER_NAME_BYTES: usize = 64;
pub const MAX_HEADER_VALUE_BYTES: usize = 1024;
pub const MAX_REQUEST_PATH_BYTES: usize = 8 * 1024;
pub const MAX_SERVICE_ID_BYTES: usize = 128;

/// One forwarded HTTP header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpHeader {
    pub name: String,
    pub value: String,
}

/// What the coordinator asks the worker to request upstream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpRequest {
    /// `GET`, `HEAD` or `POST`.
    pub method: String,
    /// An absolute path on the stats server, query included.
    pub path: String,
    #[serde(default)]
    pub headers: Vec<HttpHeader>,
    /// Standard base64. Empty when the request has no body.
    #[serde(default)]
    pub body_base64: String,
    #[serde(default)]
    pub stream: Option<StreamRequest>,
}

/// What the worker got back from the native dashboard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpResponse {
    pub status: u16,
    #[serde(default)]
    pub headers: Vec<HttpHeader>,
    /// Standard base64. Empty for a body-less response.
    #[serde(default)]
    pub body_base64: String,
    #[serde(default)]
    pub stream: Option<StreamResponse>,
}

/// The exact loopback origin a capability may reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardTarget {
    /// The address the worker dials. A wildcard bind is dialled on loopback,
    /// because that is one address the native server actually listens on.
    pub host: String,
    /// The host spelling the native server reports in its identity header.
    /// For `--host 0.0.0.0` that is still `0.0.0.0`, even though the dial is
    /// `127.0.0.1`.
    pub identity_host: String,
    pub port: u16,
}

impl DashboardTarget {
    /// The `host:port` authority an HTTP client dials.
    #[must_use]
    pub fn authority(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// The native `/stats` URL the host-service value carried.
///
/// A path, query, fragment, userinfo, control byte or malformed host is
/// refused. The destination is the exact origin the native helper reported;
/// the worker decides whether its host is a locally assigned address before
/// dialling it.
#[must_use]
pub fn parse_dashboard_origin(value: &str) -> Option<DashboardTarget> {
    let rest = value.strip_prefix("http://")?;
    let authority = rest.strip_suffix('/').unwrap_or(rest);
    if authority.is_empty()
        || authority
            .chars()
            .any(|character| matches!(character, '/' | '?' | '#' | '@'))
        || authority.bytes().any(|byte| byte <= b' ' || byte == 0x7f)
    {
        return None;
    }
    let (host, port) = authority.rsplit_once(':')?;
    if port.is_empty() || port.len() > 5 || !port.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let port = port.parse::<u16>().ok().filter(|port| *port > 0)?;
    let (dial_host, identity_host) = dial_target(host)?;
    if !dashboard_host_is_valid(&dial_host) {
        return None;
    }
    Some(DashboardTarget {
        host: dial_host,
        identity_host,
        port,
    })
}

/// Turn a dashboard `--host` spelling into the host the worker resolves or
/// dials and the spelling the native server reports in its identity header.
///
/// A wildcard bind is not overridden: the native server really listens on
/// every interface, loopback included, so dialling loopback reaches that same
/// server and the identity check still expects the wildcard it reported. Any
/// other host is kept verbatim; the worker proves it is locally assigned and
/// pins the resolved address.
fn dial_target(host: &str) -> Option<(String, String)> {
    let unbracketed = host
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(host);
    match unbracketed.to_ascii_lowercase().as_str() {
        "0.0.0.0" => Some(("127.0.0.1".to_owned(), "0.0.0.0".to_owned())),
        "::" => Some(("::1".to_owned(), "::".to_owned())),
        _ if unbracketed != host => Some((unbracketed.to_owned(), unbracketed.to_owned())),
        _ => Some((host.to_owned(), host.to_owned())),
    }
}

/// A bare host a native `--host` may legitimately name: an IP literal or a
/// DNS label sequence. This is a syntax check, not a trust decision; the
/// worker's local-assignment probe is what authorizes the destination.
#[must_use]
pub fn dashboard_host_is_valid(host: &str) -> bool {
    if host.is_empty()
        || host.len() > 253
        || host.bytes().any(|byte| {
            byte <= b' ' || byte == 0x7f || matches!(byte, b'/' | b'\\' | b'@' | b'?' | b'#')
        })
    {
        return false;
    }
    if let Some(inner) = host
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
    {
        return inner.parse::<IpAddr>().is_ok();
    }
    if host.parse::<IpAddr>().is_ok() {
        return true;
    }
    let trimmed = host.strip_suffix('.').unwrap_or(host);
    if trimmed.is_empty() {
        return false;
    }
    trimmed.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    })
}

/// A loopback host spelling, normalized to the one that is dialled.
#[must_use]
pub fn canonical_host(host: &str) -> Option<&'static str> {
    let unbracketed = host
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(host);
    match unbracketed.to_ascii_lowercase().as_str() {
        "127.0.0.1" => Some("127.0.0.1"),
        "localhost" => Some("localhost"),
        "::1" => Some("::1"),
        _ => None,
    }
}

/// Whether a native header value identifies this protocol's dashboard.
#[must_use]
pub fn is_dashboard_identity(version: Option<&str>, hostname: Option<&str>, host: &str) -> bool {
    let Some(hostname) = hostname else {
        return false;
    };
    let same_host = hostname.eq_ignore_ascii_case(host)
        // A wildcard bind is a real dashboard bound to every interface. Its
        // identity header names the wildcard while the worker dials loopback.
        || (host.eq_ignore_ascii_case("127.0.0.1") && hostname == "0.0.0.0")
        || (host == "::1" && hostname == "::");
    version == Some("3") && same_host
}

/// Every HTTP method the bridge forwards.
#[must_use]
pub fn method_is_allowed(method: &str) -> bool {
    matches!(method, "GET" | "HEAD" | "POST")
}

/// An upstream path whose only `..` is a decoded form is refused too, so the
/// native static handler never receives a traversal the proxy failed to see.
#[must_use]
pub fn path_is_safe(path: &str) -> bool {
    if path.len() > MAX_REQUEST_PATH_BYTES
        || !path.starts_with('/')
        || path
            .bytes()
            .any(|byte| byte <= b' ' || byte == 0x7f || byte == b'\\')
    {
        return false;
    }
    let (raw_path, _query) = path.split_once('?').unwrap_or((path, ""));
    let lower = raw_path.to_ascii_lowercase();
    if ["%2e", "%2f", "%5c", "%00", "%0d", "%0a"]
        .iter()
        .any(|token| lower.contains(token))
    {
        return false;
    }
    raw_path
        .split('/')
        .all(|segment| segment != "." && segment != "..")
}

/// The token and upstream path of a capability URL, or `None` when the URL is
/// not one this bridge minted.
#[must_use]
pub fn parse_capability_path(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix(CAPABILITY_PREFIX)?.strip_prefix('/')?;
    let (token, tail) = match rest.find('/') {
        Some(separator) => (&rest[..separator], &rest[separator..]),
        None => (rest, ""),
    };
    if !token_is_well_formed(token) {
        return None;
    }
    if tail.is_empty() {
        return Some((token, "/"));
    }
    path_is_safe(tail).then_some((token, tail))
}

/// The capability URL path for one token, always with the trailing slash a
/// relative `index.css` needs.
#[must_use]
pub fn capability_path(token: &str) -> String {
    format!("{CAPABILITY_PREFIX}/{token}/")
}

/// A token is 32 random bytes written as lowercase hex.
#[must_use]
pub fn token_is_well_formed(token: &str) -> bool {
    token.len() == 64
        && token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

/// Keep only the headers a capability may forward, with names lowercased and
/// every credential-bearing header dropped.
#[must_use]
pub fn sanitize_request_headers(headers: Vec<HttpHeader>) -> Vec<HttpHeader> {
    sanitize_headers(headers, ALLOWED_REQUEST_HEADERS)
}

/// Keep only the headers a browser may see, dropping `set-cookie` and
/// `location` even if a future caller accidentally asks for them.
#[must_use]
pub fn sanitize_response_headers(headers: Vec<HttpHeader>) -> Vec<HttpHeader> {
    sanitize_headers(headers, ALLOWED_RESPONSE_HEADERS)
}

fn sanitize_headers(headers: Vec<HttpHeader>, allowed: &[&str]) -> Vec<HttpHeader> {
    let mut kept: Vec<HttpHeader> = Vec::new();
    for header in headers.into_iter().take(MAX_HEADERS) {
        let name = header.name.to_ascii_lowercase();
        if !allowed.contains(&name.as_str())
            || name.len() > MAX_HEADER_NAME_BYTES
            || header.value.len() > MAX_HEADER_VALUE_BYTES
            || header
                .value
                .bytes()
                .any(|byte| byte == b'\r' || byte == b'\n' || byte == 0)
        {
            continue;
        }
        if kept.iter().any(|held| held.name == name) {
            continue;
        }
        kept.push(HttpHeader {
            name,
            value: header.value,
        });
    }
    kept
}

/// Whether a base64 body string can hold at most `limit` decoded bytes.
///
/// Computed without decoding: the caller that decodes owns its own error, and
/// the length check is the bound that keeps that decoder from allocating past
/// the ceiling.
#[must_use]
pub fn encoded_body_within(encoded: &str, limit: usize) -> bool {
    encoded.len() <= limit.saturating_mul(4).div_ceil(3).saturating_add(4)
}

#[cfg(test)]
mod tests;
