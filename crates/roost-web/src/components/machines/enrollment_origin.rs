//! Which origin a new machine is told to dial: the coordinator's declared
//! identity first, then the origin this browser is already talking to.
//!
//! Ports `workerCoordinatorUrl` of
//! `packages/protocol/src/coordinator-dial-url.ts`, which v2's deploy composer
//! and its remote-enrollment path share so a declared front door has ONE
//! precedence and ONE validation rule. An empty declaration counts as
//! undeclared; a non-blank declaration that cannot be dialled is a
//! configuration error and NEVER a reason to fall back — the operator declared
//! something, and quietly enrolling against a different door than the one they
//! named is how a fleet ends up enrolled in two places.

/// The scheme a worker may be sent over. Plain HTTP would carry a one-shot
/// enrollment grant in clear to the machine that spends it.
const REQUIRED_SCHEME: &str = "https";

/// The port the scheme already implies, so the origin a worker dials does not
/// carry it. Compared after padding is stripped, because `0443` is this port.
const DEFAULT_HTTPS_PORT: &str = "443";

/// What the discovered enrollment address permits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnrollmentDecision {
    /// A remote HTTPS root origin a worker on another machine can dial.
    Ready {
        /// The normalized origin the join command sends the worker to.
        coordinator_url: String,
    },
    /// Nothing is declared, and this browser is talking to a loopback origin.
    /// Roost is reachable from this machine and from nowhere else, which is a
    /// deployment fact and not a failure — so it draws the guide, not an error.
    LocalOnly,
    /// Something IS declared and cannot be dialled from another machine. The
    /// declared value is kept so the refusal can name what was configured.
    ConfigurationError {
        /// The declaration exactly as the coordinator reported it.
        declared_url: String,
    },
}

/// The decision for a declared identity address and this browser's own origin.
pub fn enrollment_decision(
    declared_public_url: &str,
    active_coordinator_origin: &str,
) -> EnrollmentDecision {
    let declared = declared_public_url.trim();
    if let Some(coordinator_url) = worker_dialable_origin(declared) {
        return EnrollmentDecision::Ready { coordinator_url };
    }
    if !declared.is_empty() {
        return EnrollmentDecision::ConfigurationError {
            declared_url: declared.to_owned(),
        };
    }
    match worker_dialable_origin(active_coordinator_origin) {
        Some(coordinator_url) => EnrollmentDecision::Ready { coordinator_url },
        None => EnrollmentDecision::LocalOnly,
    }
}

/// The origin a worker on ANOTHER machine can dial, normalized, or nothing.
///
/// A root origin only: a path, a query or a fragment is not part of the door,
/// and a worker that dials one reaches something else. No embedded credentials:
/// they would be pasted into a shell inside the URL. And not a loopback host,
/// which names THIS machine rather than the fleet.
pub fn worker_dialable_origin(candidate: &str) -> Option<String> {
    let (scheme, rest) = candidate.trim().split_once("://")?;
    if !scheme.eq_ignore_ascii_case(REQUIRED_SCHEME) {
        return None;
    }
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let tail = &rest[authority_end..];
    if tail.contains('?') || tail.contains('#') || (!tail.is_empty() && tail != "/") {
        return None;
    }
    let authority = &rest[..authority_end];
    if authority.contains('@') {
        return None;
    }
    let (host, port) = split_authority_port(authority)?;
    if is_loopback_host(host) {
        return None;
    }
    let host = host.to_ascii_lowercase();
    let port = match port {
        None => String::new(),
        Some(port) if port.trim_start_matches('0') == DEFAULT_HTTPS_PORT => String::new(),
        Some(port) => format!(":{port}"),
    };
    Some(format!("https://{host}{port}"))
}

/// An authority's host and optional port, refusing the spellings that cannot be
/// one: an unbracketed IPv6 literal, an empty host, and a port that is not
/// digits.
fn split_authority_port(authority: &str) -> Option<(&str, Option<&str>)> {
    let (host, port) = if authority.starts_with('[') {
        // The host KEEPS its brackets: an IPv6 literal written without them is
        // no longer an address, and the one loopback spelling that matters is
        // `[::1]`.
        let close = authority.find(']')?;
        let after = &authority[close + 1..];
        let port = if after.is_empty() {
            None
        } else {
            // Whatever follows the bracket that is not a colon and a port is not
            // an authority at all, and `strip_prefix` refusing says so.
            Some(after.strip_prefix(':')?)
        };
        (&authority[..=close], port)
    } else {
        match authority.rsplit_once(':') {
            None => (authority, None),
            // Two colons outside brackets is an IPv6 literal written without
            // them: one host, not a host and a port.
            Some((host, _port)) if host.contains(':') => return None,
            Some((host, port)) => (host, Some(port)),
        }
    };
    if host.is_empty() || !is_host(host) {
        return None;
    }
    match port {
        Some(port) if !is_port(port) => None,
        _ => Some((host, port)),
    }
}

/// The characters a host may carry. Outside brackets that is letters, digits,
/// `-` and `.`; an IPv6 literal additionally carries `:`, the embedded `.` of
/// `::ffff:10.0.0.1`, and `%` for a zone index. Anything else is not a name a
/// dialer can resolve — a space in a host is a paste accident — and a
/// non-ASCII name is refused rather than guessed at, because the guess would be
/// a punycode spelling this dialog never showed the operator.
fn is_host(host: &str) -> bool {
    let bracketed = host.starts_with('[') && host.ends_with(']');
    host.bytes().all(|byte| {
        byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'.' | b'%')
            || (bracketed && matches!(byte, b':' | b'[' | b']'))
    })
}

/// A port is at least one digit and nothing else: a trailing colon is a typo,
/// not the default port, and a port a dialer would have to guess at is not one.
fn is_port(port: &str) -> bool {
    !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit())
}

/// Whether a host names THIS machine rather than the fleet. A worker dialling it
/// from another machine reaches nothing, so a command built on it cannot work.
fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || host.starts_with("127.")
        || host == "[::1]"
}

#[cfg(test)]
mod tests {
    use super::{EnrollmentDecision, enrollment_decision, worker_dialable_origin};

    const LOOPBACK: &str = "http://127.0.0.1:4113";
    const REMOTE: &str = "https://front.example.com";

    fn ready(url: &str) -> EnrollmentDecision {
        EnrollmentDecision::Ready {
            coordinator_url: url.to_owned(),
        }
    }

    #[test]
    fn a_local_only_coordinator_offers_no_remote_enrollment_at_all() {
        for undeclared in ["", "   ", "\t"] {
            assert_eq!(
                enrollment_decision(undeclared, LOOPBACK),
                EnrollmentDecision::LocalOnly,
                "an install that declares nothing is local-only, and {undeclared:?} declares \
                 nothing"
            );
        }
        for local in [
            "http://127.0.0.1:4113",
            "https://localhost:8443",
            "https://roost.localhost",
            "https://[::1]:4103",
            "http://roost.example.com",
        ] {
            assert_eq!(
                worker_dialable_origin(local),
                None,
                "a worker on another machine cannot dial {local}"
            );
        }
    }

    /// The refusal that must never turn into a fallback.
    #[test]
    fn a_declared_address_that_cannot_be_dialled_is_a_configuration_error() {
        for declared in [
            "not-a-url",
            "http://roost.example.com",
            "https://operator:secret@roost.example.com",
            "https://roost.example.com/coord",
            "https://roost.example.com?token=1",
            "https://roost.example.com#frag",
            "https://localhost:8443",
            "https://127.0.0.1",
            "https://[::1]",
            "https://roost.example.com:port",
            "https://roost.example.com:",
        ] {
            assert_eq!(
                enrollment_decision(declared, REMOTE),
                EnrollmentDecision::ConfigurationError {
                    declared_url: declared.to_owned()
                },
                "{declared} is declared, so it is a configuration error and never a reason to \
                 dial something else"
            );
        }
    }

    #[test]
    fn a_declared_remote_address_outranks_the_origin_this_browser_dials() {
        assert_eq!(
            enrollment_decision("https://private.example.ts.net:4102", LOOPBACK),
            ready("https://private.example.ts.net:4102")
        );
        assert_eq!(enrollment_decision("", REMOTE), ready(REMOTE));
    }

    #[test]
    fn an_accepted_origin_is_normalized_to_what_a_worker_dials() {
        for (declared, expected) in [
            (
                "https://private.example.ts.net:4102/",
                "https://private.example.ts.net:4102",
            ),
            (
                "https://private.example.ts.net:443/",
                "https://private.example.ts.net",
            ),
            (
                "  https://Private.Example.TS.NET/  ",
                "https://private.example.ts.net",
            ),
            ("https://roost.example.com", "https://roost.example.com"),
            ("HTTPS://roost.example.com", "https://roost.example.com"),
            (
                "https://roost.example.com:0443",
                "https://roost.example.com",
            ),
            ("https://[2001:db8::1]:8443", "https://[2001:db8::1]:8443"),
        ] {
            assert_eq!(
                worker_dialable_origin(declared),
                Some(expected.to_owned()),
                "declared was {declared}"
            );
        }
    }
}
