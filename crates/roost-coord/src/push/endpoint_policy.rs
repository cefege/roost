//! What a push endpoint and a push key must look like before either reaches
//! the database or the network.
//!
//! Owned by the push domain. Both the subscribe RPC and the dispatch filter
//! call `endpoint_origin`, because v2's `handlers-push.ts:validateEndpoint` and
//! its `push-dispatch.ts` filter are the same four rules written twice, and two
//! copies of one admission rule is how one surface silently keeps accepting
//! what the other refuses.
//!
//! PARSED BY HAND, NOT WITH A URL CRATE, for the reason
//! `http_admission.rs:normalize_host_origins` gives: the accepted grammar is
//! exactly `https://authority[/path][?query]` and nothing else, so a general
//! parser would have to be *restricted* back down to that, which is a second
//! place to get it wrong. The four rules are scheme, userinfo, fragment, and
//! origin membership.

/// The longest endpoint accepted.
///
/// 4096 (`handlers-push.ts:20`). A push endpoint is a service-issued URL, and a
/// browser's own `pushManager.subscribe` will not produce one anywhere near
/// this long; the bound exists so a caller cannot park a megabyte in a column
/// the coordinator then hands to an HTTP client.
pub const ENDPOINT_MAX_LENGTH: usize = 4_096;

/// The longest subscription key accepted.
///
/// 512 (`handlers-push.ts:21`). A base64url P-256 public key is 87 characters
/// and an auth secret is 16, so this is generous by an order of magnitude and
/// still bounded.
pub const KEY_MAX_LENGTH: usize = 512;

/// Why an endpoint, a key, or a statement was refused.
///
/// The variants are the independent rules, kept apart so a caller can tell an
/// operator's misconfiguration from a caller probing the allowlist, and a
/// caller's bad input from the coordinator's own storage failing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PushInputError {
    /// Push is not configured, or the endpoint failed one of the four rules.
    #[error("invalid push endpoint")]
    InvalidEndpoint,
    /// A subscription key was empty, over-length, or not base64url.
    #[error("invalid push {0}")]
    InvalidKey(&'static str),
    /// No provider origin is configured, so Push is switched off.
    #[error("push is unavailable")]
    Unavailable,
    /// The device already holds [`MAX_SUBSCRIPTIONS_PER_DEVICE`](super::subscription_store::MAX_SUBSCRIPTIONS_PER_DEVICE)
    /// distinct endpoints and asked for another.
    ///
    /// The message names the cap because the browser shows it: an operator
    /// reading a support report needs to know it was the cap and not a
    /// malformed request.
    #[error(
        "push subscription limit reached ({} per device)",
        super::subscription_store::MAX_SUBSCRIPTIONS_PER_DEVICE
    )]
    DeviceCapReached,
    /// A statement failed. Never a caller's fault, so it is one variant rather
    /// than one per statement.
    #[error("push store: {0}")]
    Store(String),
}

/// The parts of an endpoint the four rules need.
#[derive(Debug, PartialEq, Eq)]
struct ParsedEndpoint {
    /// The normalized `scheme://host[:port]` a browser would report.
    origin: String,
    /// Whether the authority carried anything before an `@`.
    has_userinfo: bool,
    /// Whether a non-empty fragment followed the path.
    has_fragment: bool,
}

/// Parse an endpoint, or `None` when it is not `https://authority/...`.
///
/// The `scheme:`-without-`//` form WHATWG would resolve (`https:host/`) is
/// REFUSED rather than resolved. Refusing is safe in the direction that
/// matters: the origin still has to equal an operator-declared entry, so a
/// spelling the parser declines cannot admit an endpoint the operator did not
/// name -- it can only refuse one they did.
fn parse_endpoint(endpoint: &str) -> Option<ParsedEndpoint> {
    let (scheme, rest) = endpoint.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let tail = &rest[authority_end..];

    // WHATWG puts userinfo before the LAST `@` of the authority and
    // percent-encodes any `@` inside it, and a host can never contain a
    // literal `@`, so a literal one ahead of the path is proof -- the same
    // argument `auth/url-user-info.ts` makes.
    let (credentials, host_port) = match authority.rsplit_once('@') {
        Some((before, after)) => (Some(before), after),
        None => (None, authority),
    };
    let (host, port) = split_host_port(host_port)?;

    Some(ParsedEndpoint {
        origin: match port {
            // 443 is what `https:` already implies, so an origin that spells it
            // is the same origin and must compare equal to the bare form.
            Some(443) | None => format!("https://{host}"),
            Some(port) => format!("https://{host}:{port}"),
        },
        // An authority of `user:@host` declares an empty user and an empty
        // password, which is no credential at all; `user@host` is one.
        has_userinfo: !matches!(credentials, None | Some("") | Some(":")),
        // An empty fragment is not a fragment: v2 compares `url.hash` against
        // `""`, and WHATWG's `hash` getter returns `""` for a bare trailing
        // `#`. Porting the comparison rather than the character keeps a
        // browser-spelled endpoint admissible on both sides.
        has_fragment: tail
            .split_once('#')
            .is_some_and(|(_, fragment)| !fragment.is_empty()),
    })
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
        return Some((
            format!("[{}]", inner.to_ascii_lowercase()),
            tail_port(&after_bracket[close + 1..])?,
        ));
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
/// present one must be a number a socket can dial.
fn tail_port(tail: &str) -> Option<Option<u16>> {
    match tail {
        "" | ":" => Some(None),
        other => Some(Some(other.strip_prefix(':')?.parse().ok()?)),
    }
}

/// The origin a push endpoint resolves to, or `None` when it fails any rule
/// other than origin membership.
///
/// The membership check is separate because the two callers differ: the RPC
/// refuses with one message and the dispatch filter drops the row silently.
#[must_use]
pub fn endpoint_origin(endpoint: &str) -> Option<String> {
    if endpoint.is_empty() || endpoint.len() > ENDPOINT_MAX_LENGTH {
        return None;
    }
    let parsed = parse_endpoint(endpoint)?;
    if parsed.has_userinfo || parsed.has_fragment {
        return None;
    }
    Some(parsed.origin)
}

/// Require an endpoint whose origin the operator declared.
///
/// This is `handlers-push.ts:validateEndpoint` in full: length, parse, scheme,
/// userinfo, fragment, membership. An empty allowlist refuses everything,
/// which is the v2 "Push is unavailable" state rather than a silent accept.
pub fn validate_endpoint(
    endpoint: &str,
    allowed_origins: &[String],
) -> Result<String, PushInputError> {
    if allowed_origins.is_empty() {
        return Err(PushInputError::Unavailable);
    }
    let origin = endpoint_origin(endpoint).ok_or(PushInputError::InvalidEndpoint)?;
    if !allowed_origins.iter().any(|allowed| *allowed == origin) {
        return Err(PushInputError::InvalidEndpoint);
    }
    Ok(origin)
}

/// Require a subscription key to be unpadded base64url within the bound.
///
/// `^[A-Za-z0-9_-]+$` with a length floor of one and a ceiling of
/// [`KEY_MAX_LENGTH`] (`handlers-push.ts:22,53-59`). Checked byte-wise rather
/// than through a regex: the alphabet is 64 characters and the check runs on
/// every subscribe, so a compiled matcher would be a dependency bought for
/// one alternation.
pub fn validate_key(name: &'static str, value: &str) -> Result<(), PushInputError> {
    let acceptable = !value.is_empty()
        && value.len() <= KEY_MAX_LENGTH
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
    if acceptable {
        Ok(())
    } else {
        Err(PushInputError::InvalidKey(name))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ENDPOINT_MAX_LENGTH, KEY_MAX_LENGTH, PushInputError, endpoint_origin, validate_endpoint,
        validate_key,
    };

    fn allowlist() -> Vec<String> {
        vec!["https://push.example".to_owned()]
    }

    #[test]
    fn an_origin_is_lowercased_and_loses_the_https_default_port() {
        assert_eq!(
            endpoint_origin("https://PUSH.Example:443/subscription-token").as_deref(),
            Some("https://push.example")
        );
        assert_eq!(
            endpoint_origin("https://push.example:8443/token").as_deref(),
            Some("https://push.example:8443")
        );
    }

    #[test]
    fn userinfo_and_a_non_empty_fragment_are_refused_where_a_bare_hash_is_not() {
        assert_eq!(endpoint_origin("https://user@push.example/x"), None);
        assert_eq!(endpoint_origin("https://push.example/x#fragment"), None);
        // v2 compares `url.hash` to `""`, and WHATWG reports `""` for a bare
        // trailing `#`, so this one is admissible on both sides.
        assert_eq!(
            endpoint_origin("https://push.example/x#").as_deref(),
            Some("https://push.example")
        );
    }

    #[test]
    fn a_non_https_or_unparseable_endpoint_is_refused() {
        assert_eq!(endpoint_origin("http://push.example/x"), None);
        assert_eq!(endpoint_origin("push.example/x"), None);
        assert_eq!(endpoint_origin("https:push.example/x"), None);
        assert_eq!(endpoint_origin(""), None);
        assert_eq!(endpoint_origin(&"a".repeat(ENDPOINT_MAX_LENGTH + 1)), None);
    }

    #[test]
    fn membership_is_exact_against_the_operator_allowlist() {
        for refused in [
            "https://push.example.attacker.invalid/subscription",
            "https://127.0.0.1/subscription",
            "https://push.example:8443/subscription",
        ] {
            assert_eq!(
                validate_endpoint(refused, &allowlist()),
                Err(PushInputError::InvalidEndpoint),
                "{refused}"
            );
        }
        assert_eq!(
            validate_endpoint("https://push.example/token", &allowlist()).as_deref(),
            Ok("https://push.example")
        );
    }

    #[test]
    fn an_empty_allowlist_is_the_unavailable_state_not_a_silent_accept() {
        assert_eq!(
            validate_endpoint("https://push.example/token", &[]),
            Err(PushInputError::Unavailable)
        );
    }

    #[test]
    fn a_key_must_be_non_empty_bounded_base64url() {
        assert!(validate_key("p256dh", "abc-_ABC123").is_ok());
        assert!(validate_key("p256dh", &"a".repeat(KEY_MAX_LENGTH)).is_ok());
        assert_eq!(
            validate_key("p256dh", ""),
            Err(PushInputError::InvalidKey("p256dh"))
        );
        assert_eq!(
            validate_key("auth", "abc+def"),
            Err(PushInputError::InvalidKey("auth"))
        );
        assert_eq!(
            validate_key("auth", "abc/def"),
            Err(PushInputError::InvalidKey("auth"))
        );
        assert!(validate_key("auth", &"a".repeat(KEY_MAX_LENGTH + 1)).is_err());
    }
}
