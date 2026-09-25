//! Origin parsing and the bare-origin rules the coordinator's CORS, web push,
//! and front-door settings depend on.
//!
//! One parser, three callers. The rules are deliberately strict rather than
//! lenient: an operator who writes a value the browser will normalize into
//! something else has declared one origin and shipped another, and the
//! difference only shows up as a push subscription that silently never fires.
//! A malformed URL the WHATWG parser would have salvaged is refused instead.

use roost_protocol::{ProtocolError, ProtocolResult};

/// The port each scheme drops from an origin because it is the default.
const DEFAULT_PORTS: [(&str, u16); 2] = [("http", 80), ("https", 443)];

/// Normalize an operator-declared front door to the origin browsers will see.
///
/// An unset or empty value stays unset: the coordinator never derives a public
/// origin, because a derived one is the front door's own URL and only the
/// operator knows it.
pub fn normalize_https_origin(raw: Option<&str>, env_name: &str) -> ProtocolResult<Option<String>> {
    let Some(raw) = raw.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let Some(origin) = parse_origin(raw) else {
        return Err(ProtocolError::new(
            env_name,
            format!("{env_name} must be a valid HTTPS origin"),
        ));
    };
    if origin.scheme != "https" || origin.is_not_a_bare_https_origin() {
        return Err(ProtocolError::new(
            env_name,
            format!(
                "{env_name} must be an HTTPS origin without credentials, query, fragment, or path"
            ),
        ));
    }
    Ok(Some(origin.to_origin_string()))
}

/// Require a CORS entry to be a bare HTTP(S) origin the browser would write
/// back unchanged.
pub fn validate_bare_http_origin(origin: &str, env_name: &str) -> ProtocolResult<()> {
    // The two refusals are told apart the way the contract tells them apart, and
    // the split matters to whoever reads the log. A value with no `scheme://` at
    // all is a typo. A value that IS a URL — `file:///tmp/a` is one — is an
    // operator who meant to allow an origin and named the wrong kind of thing,
    // and "entries must be bare HTTP(S) origins" is the message that tells them
    // so. A parser that refused `file://` outright would report the typo
    // message for a real mistake.
    let looks_like_a_url = origin
        .split_once("://")
        .is_some_and(|(scheme, _)| is_scheme(scheme));
    let not_bare = || {
        ProtocolError::new(
            env_name,
            format!("{env_name} entries must be bare HTTP(S) origins: {origin}"),
        )
    };
    let Some(parsed) = parse_origin(origin) else {
        return Err(if looks_like_a_url {
            not_bare()
        } else {
            ProtocolError::new(
                env_name,
                format!("{env_name} contains an invalid origin: {origin}"),
            )
        });
    };
    let is_bare_http = parsed.scheme == "http" || parsed.scheme == "https";
    if !is_bare_http || parsed.to_origin_string() != origin {
        return Err(not_bare());
    }
    Ok(())
}

/// Require a web-push entry to be a bare HTTPS origin: a push service reached
/// over plain HTTP has already lost the subscriber keys it was handed.
pub fn validate_bare_https_origin(origin: &str, env_name: &str) -> ProtocolResult<()> {
    let Some(parsed) = parse_origin(origin) else {
        return Err(ProtocolError::new(
            env_name,
            format!("{env_name} contains an invalid origin: {origin}"),
        ));
    };
    if parsed.scheme != "https" || parsed.to_origin_string() != origin {
        return Err(ProtocolError::new(
            env_name,
            format!("{env_name} entries must be exact bare HTTPS origins: {origin}"),
        ));
    }
    Ok(())
}

/// One parsed origin: the parts a rule needs, already normalized the way a
/// browser would normalize them.
#[derive(Debug, PartialEq, Eq)]
struct Origin<'a> {
    scheme: String,
    /// Lowercased, brackets retained for an IPv6 literal.
    host: String,
    port: Option<u16>,
    /// Whether the authority carried anything before its `@`.
    has_credentials: bool,
    /// Everything after the authority: path, query, and fragment.
    tail: &'a str,
}

impl Origin<'_> {
    /// What a browser would report as `location.origin`.
    fn to_origin_string(&self) -> String {
        let is_default_port = self
            .port
            .is_some_and(|port| DEFAULT_PORTS.contains(&(self.scheme.as_str(), port)));
        match self.port {
            Some(port) if !is_default_port => format!("{}://{}:{port}", self.scheme, self.host),
            _ => format!("{}://{}", self.scheme, self.host),
        }
    }

    /// Whether anything but a bare `https://host[:port]` is present.
    fn is_not_a_bare_https_origin(&self) -> bool {
        if self.has_credentials {
            return true;
        }
        // A query or a fragment is refused in its own right. Splitting the tail
        // on them first would make `?token=secret` read as an empty path and
        // therefore a bare origin, which is how a secret ends up in a CSP and a
        // CORS allowlist entry nobody reads.
        if self.tail.contains(['?', '#']) {
            return true;
        }
        !(self.tail.is_empty() || self.tail == "/")
    }
}

/// Parse an absolute `scheme://authority` URL, or `None` when it is not one.
fn parse_origin(value: &str) -> Option<Origin<'_>> {
    let (scheme, rest) = value.split_once("://")?;
    if !is_scheme(scheme) {
        return None;
    }
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (credentials, host_port) = match rest[..authority_end].rsplit_once('@') {
        Some((before, after)) => (Some(before), after),
        None => (None, &rest[..authority_end]),
    };
    let (host, port) = split_host_port(host_port)?;
    Some(Origin {
        scheme: scheme.to_ascii_lowercase(),
        host,
        port,
        // An authority of `user:@host` declares an empty user and an empty
        // password, which is no credential at all; `user@host` is one.
        has_credentials: !matches!(credentials, None | Some("") | Some(":")),
        tail: &rest[authority_end..],
    })
}

fn is_scheme(value: &str) -> bool {
    let mut bytes = value.bytes();
    matches!(bytes.next(), Some(first) if first.is_ascii_alphabetic())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
}

/// The host and port of an authority, lowercasing the host the way a browser
/// does and dropping a port the scheme already implies.
fn split_host_port(host_port: &str) -> Option<(String, Option<u16>)> {
    if let Some(after_bracket) = host_port.strip_prefix('[') {
        let close = after_bracket.find(']')?;
        let inner = &after_bracket[..close];
        if inner.is_empty()
            || !inner
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || matches!(byte, b':' | b'.'))
        {
            return None;
        }
        let port = tail_port(&after_bracket[close + 1..])?;
        return Some((format!("[{}]", inner.to_ascii_lowercase()), port));
    }
    let (host, tail) = match host_port.split_once(':') {
        Some((host, port)) if !port.contains(':') => (host, format!(":{port}")),
        Some(_) => return None,
        None => (host_port, String::new()),
    };
    if host.is_empty() || host.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return None;
    }
    Some((host.to_ascii_lowercase(), tail_port(&tail)?))
}

/// The port in what follows a host. An absent or empty port is `None`; a
/// present one must be a number a socket can bind.
fn tail_port(tail: &str) -> Option<Option<u16>> {
    match tail {
        "" | ":" => Some(None),
        other => Some(Some(other.strip_prefix(':')?.parse().ok()?)),
    }
}

#[cfg(test)]
mod tests {
    use super::{normalize_https_origin, parse_origin, validate_bare_http_origin};

    const CORS: &str = "ROOST_CORS_ALLOWED_ORIGINS";
    const WEB_PUBLIC_URL: &str = "ROOST_WEB_PUBLIC_URL";

    #[test]
    fn a_front_door_normalizes_to_the_origin_a_browser_reports() {
        for (declared, expected) in [
            ("https://roost.example.com/", "https://roost.example.com"),
            ("https://roost.example.com", "https://roost.example.com"),
            ("https://roost.example.com:443", "https://roost.example.com"),
            (
                "https://coord.example.com:4102",
                "https://coord.example.com:4102",
            ),
            ("https://[2001:DB8::8]:8443", "https://[2001:db8::8]:8443"),
        ] {
            assert_eq!(
                normalize_https_origin(Some(declared), WEB_PUBLIC_URL)
                    .unwrap_or_else(|error| panic!("{declared}: {error}")),
                Some(expected.to_string()),
                "{declared} normalized wrong"
            );
        }
    }

    #[test]
    fn an_unset_front_door_stays_unset() {
        assert_eq!(
            normalize_https_origin(None, WEB_PUBLIC_URL),
            Ok(None),
            "an absent value must not become a derived origin"
        );
        assert_eq!(normalize_https_origin(Some(""), WEB_PUBLIC_URL), Ok(None));
    }

    #[test]
    fn anything_but_a_bare_https_origin_is_refused() {
        for value in [
            "http://roost.example.com",
            "https://roost.example.com/path",
            "https://user@roost.example.com",
            "https://roost.example.com?token=secret",
            "https://roost.example.com#fragment",
            "not a URL",
            "https://",
            "https://roost.example.com:70000",
        ] {
            assert!(
                normalize_https_origin(Some(value), WEB_PUBLIC_URL).is_err(),
                "{value} was accepted"
            );
        }
    }

    #[test]
    fn a_cors_entry_must_be_written_exactly_as_a_browser_would() {
        for good in [
            "http://localhost:3000",
            "https://example.com",
            "https://[2001:db8::8]:8443",
        ] {
            assert!(
                validate_bare_http_origin(good, CORS).is_ok(),
                "{good} was refused"
            );
        }
        for bad in [
            "https://example.com/",
            "https://example.com/path",
            "https://Example.com",
            "https://example.com:443",
            "file:///tmp/a",
            "not a URL",
        ] {
            assert!(
                validate_bare_http_origin(bad, CORS).is_err(),
                "{bad} was accepted"
            );
        }
    }

    #[test]
    fn an_empty_userinfo_is_no_credential() {
        let parsed = parse_origin("https://:@roost.example.com");
        let origin = match parsed {
            Some(origin) => origin,
            None => panic!("a bare https origin must parse"),
        };
        assert!(!origin.has_credentials);
        assert_eq!(origin.to_origin_string(), "https://roost.example.com");
    }
}
