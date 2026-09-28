//! The response security headers a roost HTTP listener sends, from one CSP
//! builder so two doors serving one SPA bundle cannot drift into different
//! `connect-src` rules (v2 `packages/host/src/http-security.ts`). Called by the
//! worker's loopback door (`roost-worker` `runtime::door_serve`). Depends on
//! `std` alone.

/// The directive every policy ends with: no roost page may be framed.
const CSP_TAIL: &str = "frame-ancestors 'none'";

/// The `Content-Security-Policy` for a page that may connect to itself and to
/// `connect_origins`. Each source is listed once, in first-seen order;
/// `relaxed` additionally admits every plaintext HTTP and WebSocket endpoint.
#[must_use]
pub fn build_csp(relaxed: bool, connect_origins: &[String]) -> String {
    let relaxed_sources: &[&str] = if relaxed { &["http:", "ws:"] } else { &[] };
    let mut connections: Vec<&str> = vec!["'self'"];
    for source in connect_origins
        .iter()
        .map(String::as_str)
        .chain(relaxed_sources.iter().copied())
    {
        if !connections.contains(&source) {
            connections.push(source);
        }
    }
    format!(
        "default-src 'self'; \
         script-src 'self' 'wasm-unsafe-eval' blob:; \
         worker-src 'self' blob:; \
         style-src 'self' 'unsafe-inline'; \
         img-src 'self' data: blob:; \
         font-src 'self' data:; \
         base-uri 'self'; \
         form-action 'none'; \
         object-src 'none'; \
         connect-src {}; \
         {CSP_TAIL}",
        connections.join(" ")
    )
}

/// Every security header a response carries, as lowercase names and values
/// (v2 `applySecurityHeaders`). HSTS only when `hsts`: a listener that does
/// not know TLS terminates in front of it must not tell a browser it does.
#[must_use]
pub fn security_headers(
    relaxed: bool,
    hsts: bool,
    connect_origins: &[String],
) -> Vec<(&'static str, String)> {
    let mut headers = vec![
        (
            "content-security-policy",
            build_csp(relaxed, connect_origins),
        ),
        ("x-frame-options", "DENY".to_owned()),
        ("x-content-type-options", "nosniff".to_owned()),
        ("referrer-policy", "no-referrer".to_owned()),
        (
            "permissions-policy",
            "camera=(), geolocation=(), microphone=(self)".to_owned(),
        ),
    ];
    if hsts {
        headers.push(("strict-transport-security", "max-age=31536000".to_owned()));
    }
    headers
}

#[cfg(test)]
mod tests {
    use super::{build_csp, security_headers};

    /// A repeated source is listed once, where it first appeared, and the
    /// relaxed plaintext sources come after every declared one.
    #[test]
    fn connect_sources_are_deduplicated_in_first_seen_order() {
        let origins = vec![
            "http://coord.test".to_owned(),
            "ws://coord.test".to_owned(),
            "http://coord.test".to_owned(),
            "http:".to_owned(),
        ];
        assert!(build_csp(false, &origins).contains(
            "connect-src 'self' http://coord.test ws://coord.test http:; frame-ancestors 'none'"
        ));
        assert!(build_csp(true, &origins).contains(
            "connect-src 'self' http://coord.test ws://coord.test http: ws:; frame-ancestors"
        ));
    }

    /// HSTS rides only on a listener that asked for it.
    #[test]
    fn hsts_is_sent_only_when_asked_for() {
        let named = |hsts| -> Vec<&str> {
            security_headers(false, hsts, &[])
                .into_iter()
                .map(|(name, _)| name)
                .collect()
        };
        assert!(!named(false).contains(&"strict-transport-security"));
        assert!(named(true).contains(&"strict-transport-security"));
        assert!(named(false).contains(&"x-frame-options"));
    }
}
