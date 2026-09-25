//! Which ICE candidates the direct carrier is willing to accept.
//!
//! A candidate is the one place an SDP can name an address the grant never
//! authorized, so every field is checked rather than trimmed: the protocol must
//! be UDP, the type must be one this carrier can actually use, and the address
//! must be a unicast address a browser can plausibly have. Loopback,
//! unspecified, multicast, and link-local shapes are refused because none of
//! them is a peer.

use super::{TerminalPeerSdpError, TerminalPeerSdpErrorCode};
use crate::terminal_peer::peer::is_canonical_decimal;

/// The candidate kinds this carrier admits. A relay is refused rather than
/// filtered: a TURN relay would put the peer connection outside the address
/// range the grant authorized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TerminalPeerCandidateType {
    Host,
    Srflx,
    Prflx,
}

impl TerminalPeerCandidateType {
    /// The order metadata reports types in, whatever order they arrived in.
    pub const ORDER: [Self; 3] = [Self::Host, Self::Srflx, Self::Prflx];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Srflx => "srflx",
            Self::Prflx => "prflx",
        }
    }
}

pub(super) fn candidate_error(detail: &str) -> TerminalPeerSdpError {
    TerminalPeerSdpError::with_detail(TerminalPeerSdpErrorCode::Candidate, detail)
}

pub(super) fn inspect_candidate(
    value: &str,
) -> Result<TerminalPeerCandidateType, TerminalPeerSdpError> {
    let fields: Vec<&str> = value.split(' ').collect();
    if fields.len() < 8 || fields.iter().any(|field| field.is_empty()) {
        return Err(candidate_error("shape"));
    }
    if !fields[0].bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
        return Err(candidate_error("foundation"));
    }
    if fields[1] != "1" {
        return Err(candidate_error("component"));
    }
    if !fields[2].eq_ignore_ascii_case("udp") {
        return Err(candidate_error("protocol"));
    }
    if !is_bounded_decimal(fields[3], 9, 0, 0xffff_ffff) {
        return Err(candidate_error("priority"));
    }
    if !is_candidate_address(fields[4]) {
        return Err(candidate_error("address"));
    }
    if !is_port(fields[5]) {
        return Err(candidate_error("port"));
    }
    if !fields[6].eq_ignore_ascii_case("typ") {
        return Err(candidate_error("marker"));
    }
    let candidate_type = match fields[7].to_ascii_lowercase().as_str() {
        "host" => TerminalPeerCandidateType::Host,
        "srflx" => TerminalPeerCandidateType::Srflx,
        "prflx" => TerminalPeerCandidateType::Prflx,
        _ => return Err(candidate_error("type")),
    };
    if !(fields.len() - 8).is_multiple_of(2) {
        return Err(candidate_error("extensions"));
    }
    let mut related_address: Option<&str> = None;
    let mut related_port: Option<&str> = None;
    for pair in fields[8..].chunks(2) {
        match pair[0].to_ascii_lowercase().as_str() {
            "tcptype" => return Err(candidate_error("tcp")),
            "raddr" if related_address.is_some() => return Err(duplicate_extension()),
            "raddr" => related_address = Some(pair[1]),
            "rport" if related_port.is_some() => return Err(duplicate_extension()),
            "rport" => related_port = Some(pair[1]),
            _ => {}
        }
    }
    // A related address of "unspecified" is how a browser redacts a host
    // candidate's reflexive address, so that pair is allowed to be
    // meaningless; a named one still has to parse.
    let redacted = matches!(related_address, Some("0.0.0.0") | Some("::"))
        && matches!(related_port, Some("0") | Some("9"));
    if (related_address.is_some() || related_port.is_some())
        && !redacted
        && !(related_address.is_some_and(is_candidate_address) && related_port.is_some_and(is_port))
    {
        return Err(candidate_error("related"));
    }
    Ok(candidate_type)
}

fn duplicate_extension() -> TerminalPeerSdpError {
    TerminalPeerSdpError::new(TerminalPeerSdpErrorCode::Candidate)
}

pub(super) fn is_port(value: &str) -> bool {
    is_bounded_decimal(value, 4, 1, 65_535)
}

fn is_bounded_decimal(value: &str, max_digits: usize, low: u64, high: u64) -> bool {
    is_canonical_decimal(value, max_digits)
        && value
            .parse::<u64>()
            .is_ok_and(|number| (low..=high).contains(&number))
}

fn is_candidate_address(value: &str) -> bool {
    if is_mdns_hostname(value) {
        return true;
    }
    if let Some(octets) = parse_ipv4(value) {
        return is_ipv4_unicast(octets);
    }
    let Some(words) = parse_ipv6(value) else {
        return false;
    };
    if words[0] & 0xff00 == 0xff00 {
        return false;
    }
    if words.iter().all(|word| *word == 0) {
        return false;
    }
    // An IPv4-mapped address is a v4 address wearing a v6 hat, so it is held to
    // the IPv4 unicast rule instead of passing as a v6 address.
    if words[0] == 0 && words[5] == 0xffff {
        return is_ipv4_unicast([
            (words[6] >> 8) as u8,
            (words[6] & 0xff) as u8,
            (words[7] >> 8) as u8,
            (words[7] & 0xff) as u8,
        ]);
    }
    true
}

fn is_mdns_hostname(value: &str) -> bool {
    if value.len() <= ".local".len() || !value.to_ascii_lowercase().ends_with(".local") {
        return false;
    }
    value.split('.').all(|label| {
        let bytes = label.as_bytes();
        !bytes.is_empty()
            && bytes.len() <= 63
            && bytes[0].is_ascii_alphanumeric()
            && bytes[bytes.len() - 1].is_ascii_alphanumeric()
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
    })
}

fn parse_ipv4(value: &str) -> Option<[u8; 4]> {
    let mut octets = [0u8; 4];
    let mut count = 0usize;
    for part in value.split('.') {
        count += 1;
        if count > 4 || !is_canonical_decimal(part, 2) {
            return None;
        }
        octets[count - 1] = u8::try_from(part.parse::<u32>().ok()?).ok()?;
    }
    if count == 4 { Some(octets) } else { None }
}

fn is_ipv4_unicast(octets: [u8; 4]) -> bool {
    octets[0] != 0 && octets[0] < 224
}

fn parse_ipv6(value: &str) -> Option<[u16; 8]> {
    if value.is_empty() || value.contains(['%', '[', ']']) {
        return None;
    }
    let halves: Vec<&str> = value.split("::").collect();
    if halves.len() > 2 {
        return None;
    }
    let raw_words: Vec<&str> = halves
        .iter()
        .filter(|half| !half.is_empty())
        .flat_map(|half| half.split(':'))
        .collect();
    let mut words: Vec<u16> = Vec::with_capacity(8);
    for (index, raw_word) in raw_words.iter().enumerate() {
        if raw_word.contains('.') {
            if index + 1 != raw_words.len() || !value.ends_with(raw_word) {
                return None;
            }
            let embedded = parse_ipv4(raw_word)?;
            words.push(u16::from(embedded[0]) << 8 | u16::from(embedded[1]));
            words.push(u16::from(embedded[2]) << 8 | u16::from(embedded[3]));
            continue;
        }
        let short_word = !raw_word.is_empty() && raw_word.len() <= 4;
        if !short_word || !raw_word.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        words.push(u16::from_str_radix(raw_word, 16).ok()?);
    }
    if halves.len() == 1 {
        return words.try_into().ok();
    }
    if words.len() >= 8 {
        return None;
    }
    let left = if halves[0].is_empty() {
        0
    } else {
        halves[0].split(':').count()
    };
    let mut expanded = [0u16; 8];
    expanded[..left].copy_from_slice(&words[..left]);
    expanded[left + (8 - words.len())..].copy_from_slice(&words[left..]);
    Some(expanded)
}
