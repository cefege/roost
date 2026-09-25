//! The operator-declared STUN list that both ends of a direct carrier share.
//!
//! This is the one place a peer connection's server can be chosen, so the list
//! is normalized to exactly one spelling per server or refused: a relay
//! scheme, a credential, a path, a second spelling of one server, or a control
//! character would all reach a browser that gathers ICE, and none of them may
//! arrive that way from configuration.

use crate::error::{ProtocolError, ProtocolResult};
use crate::terminal_peer::peer::{
    DEFAULT_TERMINAL_PEER_STUN_URLS, TERMINAL_PEER_STUN_URL_MAX_COUNT, TERMINAL_PEER_STUN_URLS_ENV,
};

/// Parses only operator-declared, non-relay STUN URLs suitable for both peers.
///
/// An absent setting resolves to the account-free default and an empty one to
/// no server at all. Everything else must normalize to one distinct `stun:`
/// URL: this list is the only place a relay could be introduced.
pub fn parse_terminal_peer_stun_urls(raw: Option<&str>) -> ProtocolResult<Vec<String>> {
    let Some(raw) = raw else {
        return Ok(DEFAULT_TERMINAL_PEER_STUN_URLS
            .iter()
            .map(|url| (*url).to_owned())
            .collect());
    };
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    if raw
        .chars()
        .any(|character| character <= '\u{1f}' || character == '\u{7f}')
    {
        return Err(stun_url_error());
    }
    let urls: Vec<&str> = raw.split(',').collect();
    if urls.len() > TERMINAL_PEER_STUN_URL_MAX_COUNT {
        return Err(stun_list_error());
    }
    let mut normalized: Vec<String> = Vec::with_capacity(urls.len());
    for url in urls {
        let Some(value) = normalize_terminal_peer_stun_url(url) else {
            return Err(stun_url_error());
        };
        if normalized.contains(&value) {
            return Err(stun_list_error());
        }
        normalized.push(value);
    }
    Ok(normalized)
}

fn stun_list_error() -> ProtocolError {
    let reason = format!(
        "{TERMINAL_PEER_STUN_URLS_ENV} must contain 1 to \
         {TERMINAL_PEER_STUN_URL_MAX_COUNT} distinct stun: UDP URLs"
    );
    ProtocolError::new("terminal_peer.stun_urls", reason)
}

fn stun_url_error() -> ProtocolError {
    let reason = format!("{TERMINAL_PEER_STUN_URLS_ENV} contains an invalid STUN URL");
    ProtocolError::new("terminal_peer.stun_urls", reason)
}

/// One `stun:` URL in its single wire spelling. A credential, path, query, or
/// fragment is refused: a URL that can carry a credential leaks into a log.
fn normalize_terminal_peer_stun_url(value: &str) -> Option<String> {
    if value.is_empty() || value.trim() != value || value.contains(['/', '?', '#', '@', '\\']) {
        return None;
    }
    if !value
        .get(..5)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("stun:"))
    {
        return None;
    }
    let authority = value.get(5..)?;
    if authority.is_empty() {
        return None;
    }
    if let Some(bracketed) = authority.strip_prefix('[') {
        let close = bracketed.find(']')?;
        let host = &bracketed[..close];
        let after = &bracketed[close + 1..];
        let port = if after.is_empty() {
            None
        } else {
            let port = after.strip_prefix(':')?;
            if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            Some(port)
        };
        if !is_ipv6_address(host) {
            return None;
        }
        let host = host.to_ascii_lowercase();
        return Some(format!("stun:[{host}]{}", normalize_port(port)?));
    }
    if authority.contains(':') {
        let colon = authority.rfind(':')?;
        let host = &authority[..colon];
        let port = &authority[colon + 1..];
        if host.is_empty() || port.is_empty() || !(is_ipv4_address(host) || is_dns_name(host)) {
            return None;
        }
        let host = host.to_ascii_lowercase();
        return Some(format!("stun:{host}{}", normalize_port(Some(port))?));
    }
    if !is_ipv4_address(authority) && !is_dns_name(authority) {
        return None;
    }
    Some(format!("stun:{}", authority.to_ascii_lowercase()))
}

/// The optional `:port` suffix. An absent port stays absent: neither end is
/// told a default the other will not derive.
fn normalize_port(raw: Option<&str>) -> Option<String> {
    let Some(raw) = raw else {
        return Some(String::new());
    };
    if !is_canonical_decimal(raw, 4) {
        return None;
    }
    let port: u32 = raw.parse().ok()?;
    if !(1..=65_535).contains(&port) {
        return None;
    }
    Some(format!(":{port}"))
}

/// `^(?:0|[1-9]\d{0,max_digits})$`: the no-leading-zero decimal the carrier's
/// URLs use, so one server cannot be spelled two ways and the duplicate check
/// cannot be evaded by padding.
pub(crate) fn is_canonical_decimal(raw: &str, max_digits: usize) -> bool {
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    if raw.as_bytes()[0] == b'0' {
        return raw.len() == 1;
    }
    raw.len() <= max_digits + 1
}

fn is_dns_name(value: &str) -> bool {
    if value.len() > 253 {
        return false;
    }
    // An all-numeric name is never a DNS name, so a mistyped address cannot resolve.
    let numeric = value
        .split('.')
        .all(|part| part.bytes().all(|byte| byte.is_ascii_digit()));
    !numeric && value.split('.').all(is_dns_label)
}

fn is_dns_label(label: &str) -> bool {
    let bytes = label.as_bytes();
    let is_label_byte = |byte: u8| byte.is_ascii_alphanumeric();
    if bytes.is_empty() || bytes.len() > 63 {
        return false;
    }
    is_label_byte(bytes[0])
        && is_label_byte(bytes[bytes.len() - 1])
        && bytes
            .iter()
            .all(|byte| is_label_byte(*byte) || *byte == b'-')
}
fn is_ipv4_address(value: &str) -> bool {
    let mut octets = 0usize;
    for octet in value.split('.') {
        octets += 1;
        let valid = is_canonical_decimal(octet, 2)
            && octet.parse::<u32>().is_ok_and(|number| number <= 255);
        if octets > 4 || !valid {
            return false;
        }
    }
    octets == 4
}

fn is_ipv6_address(value: &str) -> bool {
    if value.is_empty() || value.contains(['%', '[', ']']) {
        return false;
    }
    let double_colon = value.find("::");
    if double_colon.is_some() && value.rfind("::") != double_colon {
        return false;
    }
    let units: Vec<&str> = value
        .split("::")
        .filter(|part| !part.is_empty())
        .flat_map(|part| part.split(':'))
        .collect();
    if units.is_empty() {
        return double_colon.is_some();
    }
    let mut unit_count = 0usize;
    for (index, unit) in units.iter().enumerate() {
        // A dotted tail is the only place an embedded IPv4 address is legal.
        if unit.contains('.') {
            if index + 1 != units.len() || !value.ends_with(unit) || !is_ipv4_address(unit) {
                return false;
            }
            unit_count += 2;
            continue;
        }
        if unit.is_empty() || unit.len() > 4 || !unit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return false;
        }
        unit_count += 1;
    }
    match double_colon {
        None => unit_count == 8,
        Some(_) => unit_count < 8,
    }
}
