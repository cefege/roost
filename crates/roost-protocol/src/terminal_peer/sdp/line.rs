//! Reading one SDP line, and the scope its value lands in.
//!
//! The session scope and the media scope are kept apart because SDP allows an
//! attribute in either and this carrier must know which one it read: a
//! fingerprint stated twice at different levels has to be the same
//! certificate, or the two ends are pinning different things.

use super::{TerminalPeerSdpError, TerminalPeerSdpErrorCode};
use crate::terminal_peer::peer::{
    TERMINAL_PEER_SDP_ICE_PASSWORD_MAX_UTF8_BYTES, TERMINAL_PEER_SDP_ICE_PASSWORD_MIN_UTF8_BYTES,
    TERMINAL_PEER_SDP_ICE_UFRAG_MAX_UTF8_BYTES, TERMINAL_PEER_SDP_MAX_LINE_UTF8_BYTES,
};

use super::candidate::is_port;

pub(super) const SESSION: usize = 0;
pub(super) const APPLICATION: usize = 1;
const FINGERPRINT_PREFIX: &str = "a=fingerprint:sha-256 ";

/// What one SDP level has stated so far. An attribute this carrier requires
/// stays `None` until it is read, so its absence is distinguishable from a
/// value that was refused.
#[derive(Debug, Default, Clone)]
pub(super) struct SdpScope {
    pub fingerprint_sha256: Option<String>,
    pub ice_ufrag: Option<String>,
    pub ice_password: Option<String>,
    pub max_message_size: Option<u64>,
}

/// Splits on CRLF, a lone CR, or a lone LF, and drops the empty line a
/// trailing newline leaves behind.
pub(super) fn split_sdp_lines(sdp: &str) -> Vec<&str> {
    let bytes = sdp.as_bytes();
    let mut lines = Vec::new();
    let mut start = 0usize;
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'\r' | b'\n' => {
                lines.push(&sdp[start..index]);
                let paired = bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n');
                index += if paired { 2 } else { 1 };
                start = index;
            }
            _ => index += 1,
        }
    }
    if start < bytes.len() {
        lines.push(&sdp[start..]);
    }
    lines
}

pub(super) fn is_valid_sdp_line(line: &str) -> bool {
    if line.is_empty() || line.len() > TERMINAL_PEER_SDP_MAX_LINE_UTF8_BYTES {
        return false;
    }
    let bytes = line.as_bytes();
    bytes[0].is_ascii_lowercase()
        && bytes.get(1) == Some(&b'=')
        && bytes.iter().all(|byte| (0x20..=0x7e).contains(byte))
}

pub(super) fn is_application_data_channel_line(section: &str) -> bool {
    let fields: Vec<&str> = section.split(' ').collect();
    fields.len() == 4
        && fields[0] == "application"
        && is_port(fields[1])
        && fields[2].eq_ignore_ascii_case("udp/dtls/sctp")
        && fields[3] == "webrtc-datachannel"
}

pub(super) fn is_valid_dtls_setup(value: &str) -> bool {
    matches!(value, "actpass" | "active" | "passive")
}

pub(super) fn assign_fingerprint(
    scopes: &mut [SdpScope],
    active: usize,
    line: &str,
) -> Result<(), TerminalPeerSdpError> {
    let refused = || TerminalPeerSdpError::new(TerminalPeerSdpErrorCode::Fingerprint);
    let Some(value) = strip_prefix_ignoring_case(line, FINGERPRINT_PREFIX) else {
        return Err(refused());
    };
    if !super::is_colon_form_sha256(value) {
        return Err(refused());
    }
    let normalized =
        super::normalize_terminal_peer_sha256_fingerprint(value).ok_or_else(refused)?;
    assign_once(
        &mut scopes[active].fingerprint_sha256,
        &normalized,
        TerminalPeerSdpErrorCode::Fingerprint,
    )
}

pub(super) fn assign_ice_ufrag(
    scopes: &mut [SdpScope],
    active: usize,
    value: &str,
) -> Result<(), TerminalPeerSdpError> {
    if !is_ice_value(value, 1, TERMINAL_PEER_SDP_ICE_UFRAG_MAX_UTF8_BYTES) {
        return Err(TerminalPeerSdpError::new(
            TerminalPeerSdpErrorCode::IceCredentials,
        ));
    }
    assign_once(
        &mut scopes[active].ice_ufrag,
        value,
        TerminalPeerSdpErrorCode::IceCredentials,
    )
}

pub(super) fn assign_ice_password(
    scopes: &mut [SdpScope],
    active: usize,
    value: &str,
) -> Result<(), TerminalPeerSdpError> {
    if !is_ice_value(
        value,
        TERMINAL_PEER_SDP_ICE_PASSWORD_MIN_UTF8_BYTES,
        TERMINAL_PEER_SDP_ICE_PASSWORD_MAX_UTF8_BYTES,
    ) {
        return Err(TerminalPeerSdpError::new(
            TerminalPeerSdpErrorCode::IceCredentials,
        ));
    }
    assign_once(
        &mut scopes[active].ice_password,
        value,
        TerminalPeerSdpErrorCode::IceCredentials,
    )
}

pub(super) fn assign_max_message_size(
    scopes: &mut [SdpScope],
    active: usize,
    value: &str,
) -> Result<(), TerminalPeerSdpError> {
    let refused = || TerminalPeerSdpError::new(TerminalPeerSdpErrorCode::MaxMessageSize);
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(refused());
    }
    // RFC 8831 carries this as a JavaScript number, so a value past 2^53 is
    // not a safe integer and cannot be compared across the two ends.
    let Ok(max_message_size) = value.parse::<u64>() else {
        return Err(refused());
    };
    if max_message_size > (1_u64 << 53) - 1 {
        return Err(refused());
    }
    let slot = &mut scopes[active].max_message_size;
    if slot.is_some_and(|existing| existing != max_message_size) {
        return Err(refused());
    }
    *slot = Some(max_message_size);
    Ok(())
}

/// A scope may repeat one of these attributes, but never with a second value:
/// two fingerprints or two passwords in one scope is a peer choosing which one
/// the connection actually uses.
fn assign_once(
    slot: &mut Option<String>,
    value: &str,
    code: TerminalPeerSdpErrorCode,
) -> Result<(), TerminalPeerSdpError> {
    if slot.as_deref().is_some_and(|existing| existing != value) {
        return Err(TerminalPeerSdpError::new(code));
    }
    *slot = Some(value.to_string());
    Ok(())
}

/// ICE values are printable ASCII, so the byte count the caps are written in
/// is also their character count.
fn is_ice_value(value: &str, min_bytes: usize, max_bytes: usize) -> bool {
    value.len() >= min_bytes
        && value.len() <= max_bytes
        && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

/// `line` with `prefix` removed, matching the prefix case-insensitively. The
/// SDP attribute name is case-insensitive per RFC 4566, and a browser that
/// spells it `a=fingerprint` where the contract said `a=Fingerprint` is still
/// the same attribute.
fn strip_prefix_ignoring_case<'line>(line: &'line str, prefix: &str) -> Option<&'line str> {
    let head = line.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &line[prefix.len()..])
}
