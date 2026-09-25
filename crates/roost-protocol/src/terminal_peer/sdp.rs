//! Inspecting a browser's SDP answer, filtering its candidates, and
//! normalizing the certificate fingerprint that identifies the far end.
//!
//! Signaling is the first untrusted input the direct path sees, so an offer or
//! an answer is bounded and read line by line before any ICE processing
//! starts: exactly one UDP/DTLS/SCTP data-channel media section, one SHA-256
//! fingerprint, and candidates that cannot move the authorized reachability
//! boundary. Nothing extracted here carries a credential or an address back to
//! the caller — the metadata is what a peer is fenced on.
//!
//! One line is read by `line`, one candidate is judged by `candidate`, and this
//! module holds the two ends: what an answer is allowed to say, and the
//! metadata a caller may fence on.

use std::fmt;

mod candidate;
mod line;

use crate::terminal_peer::peer::{
    TERMINAL_PEER_MAX_MESSAGE_SIZE, TERMINAL_PEER_SDP_DEFAULT_MAX_MESSAGE_SIZE,
    TERMINAL_PEER_SDP_MAX_CANDIDATES, TERMINAL_PEER_SDP_MAX_LINES,
    TERMINAL_PEER_SDP_MAX_UTF8_BYTES,
};
pub use candidate::TerminalPeerCandidateType;
use candidate::inspect_candidate;
use line::{
    APPLICATION, SESSION, SdpScope, assign_fingerprint, assign_ice_password, assign_ice_ufrag,
    assign_max_message_size, is_application_data_channel_line, is_valid_dtls_setup,
    is_valid_sdp_line, split_sdp_lines,
};

/// Why an offer or answer was refused. Every code is a fixed string and the
/// message never quotes the SDP: a rejection is logged, and a logged SDP is a
/// logged ICE password.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TerminalPeerSdpErrorCode {
    SdpSize,
    SdpLines,
    SdpLine,
    Media,
    Fingerprint,
    IceCredentials,
    MaxMessageSize,
    CandidateCount,
    Candidate,
}

impl TerminalPeerSdpErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SdpSize => "sdp-size",
            Self::SdpLines => "sdp-lines",
            Self::SdpLine => "sdp-line",
            Self::Media => "media",
            Self::Fingerprint => "fingerprint",
            Self::IceCredentials => "ice-credentials",
            Self::MaxMessageSize => "max-message-size",
            Self::CandidateCount => "candidate-count",
            Self::Candidate => "candidate",
        }
    }
}

/// A refused offer or answer. `detail` names which rule inside the code
/// failed, never the value that failed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalPeerSdpError {
    pub code: TerminalPeerSdpErrorCode,
    pub detail: Option<String>,
}

impl TerminalPeerSdpError {
    pub fn new(code: TerminalPeerSdpErrorCode) -> Self {
        Self { code, detail: None }
    }

    pub fn with_detail(code: TerminalPeerSdpErrorCode, detail: &str) -> Self {
        Self {
            code,
            detail: Some(detail.to_string()),
        }
    }
}

impl fmt::Display for TerminalPeerSdpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = self.code.as_str();
        match &self.detail {
            Some(detail) => write!(formatter, "terminal peer SDP rejected: {code}:{detail}"),
            None => write!(formatter, "terminal peer SDP rejected: {code}"),
        }
    }
}

impl std::error::Error for TerminalPeerSdpError {}

/// What a caller may fence on from an accepted offer or answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalPeerSdpMetadata {
    /// Lowercase, colon-free SHA-256 DTLS fingerprint.
    pub fingerprint_sha256: String,
    pub candidate_count: usize,
    pub candidate_types: Vec<TerminalPeerCandidateType>,
    /// `None` is SDP's explicit unlimited value, not an absent one.
    pub max_message_size: Option<u64>,
}

const CANDIDATE_PREFIX: &str = "a=candidate:";

/// Inspects an offer or answer, or refuses it with a fixed code.
pub fn inspect_terminal_peer_sdp(
    sdp: &str,
) -> Result<TerminalPeerSdpMetadata, TerminalPeerSdpError> {
    if sdp.len() > TERMINAL_PEER_SDP_MAX_UTF8_BYTES {
        return Err(TerminalPeerSdpError::new(TerminalPeerSdpErrorCode::SdpSize));
    }
    let mut lines = split_sdp_lines(sdp);
    if lines.last() == Some(&"") {
        lines.pop();
    }
    if lines.is_empty() || lines.len() > TERMINAL_PEER_SDP_MAX_LINES {
        return Err(TerminalPeerSdpError::new(
            TerminalPeerSdpErrorCode::SdpLines,
        ));
    }
    let mut scopes = [SdpScope::default(), SdpScope::default()];
    let mut active = SESSION;
    let mut has_application = false;
    let mut candidate_count = 0usize;
    let mut candidate_types: Vec<TerminalPeerCandidateType> = Vec::new();

    for line in lines {
        if !is_valid_sdp_line(line) {
            return Err(TerminalPeerSdpError::new(TerminalPeerSdpErrorCode::SdpLine));
        }
        if let Some(section) = line.strip_prefix("m=") {
            if has_application || !is_application_data_channel_line(section) {
                return Err(TerminalPeerSdpError::new(TerminalPeerSdpErrorCode::Media));
            }
            has_application = true;
            active = APPLICATION;
            continue;
        }
        if !line.starts_with("a=") {
            continue;
        }
        if line.starts_with("a=fingerprint:") {
            assign_fingerprint(&mut scopes, active, line)?;
        } else if let Some(value) = line.strip_prefix("a=ice-ufrag:") {
            assign_ice_ufrag(&mut scopes, active, value)?;
        } else if let Some(value) = line.strip_prefix("a=ice-pwd:") {
            assign_ice_password(&mut scopes, active, value)?;
        } else if let Some(value) = line.strip_prefix("a=max-message-size:") {
            assign_max_message_size(&mut scopes, active, value)?;
        } else if let Some(value) = line.strip_prefix(CANDIDATE_PREFIX) {
            if !has_application {
                return Err(TerminalPeerSdpError::new(
                    TerminalPeerSdpErrorCode::Candidate,
                ));
            }
            if candidate_count >= TERMINAL_PEER_SDP_MAX_CANDIDATES {
                return Err(TerminalPeerSdpError::new(
                    TerminalPeerSdpErrorCode::CandidateCount,
                ));
            }
            let kind = inspect_candidate(value)?;
            if !candidate_types.contains(&kind) {
                candidate_types.push(kind);
            }
            candidate_count += 1;
        } else if line.starts_with("a=remote-candidates:") || line.starts_with("a=crypto:") {
            return Err(TerminalPeerSdpError::new(
                TerminalPeerSdpErrorCode::Candidate,
            ));
        } else if let Some(value) = line.strip_prefix("a=setup:")
            && !is_valid_dtls_setup(value)
        {
            return Err(TerminalPeerSdpError::new(TerminalPeerSdpErrorCode::Media));
        }
    }
    resolve_metadata(&scopes, has_application, candidate_count, candidate_types)
}

/// The session-level value stands only where the media section carries none,
/// and a value neither level carries is a refusal: a peer that offered no
/// fingerprint is not a peer whose certificate can be pinned.
fn resolve_metadata(
    scopes: &[SdpScope; 2],
    has_application: bool,
    candidate_count: usize,
    candidate_types: Vec<TerminalPeerCandidateType>,
) -> Result<TerminalPeerSdpMetadata, TerminalPeerSdpError> {
    if !has_application {
        return Err(TerminalPeerSdpError::new(TerminalPeerSdpErrorCode::Media));
    }
    let session = &scopes[SESSION];
    let application = &scopes[APPLICATION];
    let conflict = matches!(
        (session.fingerprint_sha256.as_ref(), application.fingerprint_sha256.as_ref()),
        (Some(session_fingerprint), Some(application_fingerprint))
            if session_fingerprint != application_fingerprint
    );
    if conflict {
        return Err(TerminalPeerSdpError::new(
            TerminalPeerSdpErrorCode::Fingerprint,
        ));
    }
    let fingerprint_sha256 = effective(
        session.fingerprint_sha256.as_deref(),
        application.fingerprint_sha256.as_deref(),
        TerminalPeerSdpErrorCode::Fingerprint,
    )?
    .to_string();
    effective(
        session.ice_ufrag.as_deref(),
        application.ice_ufrag.as_deref(),
        TerminalPeerSdpErrorCode::IceCredentials,
    )?;
    effective(
        session.ice_password.as_deref(),
        application.ice_password.as_deref(),
        TerminalPeerSdpErrorCode::IceCredentials,
    )?;
    let max_message_size = match application.max_message_size.or(session.max_message_size) {
        None => Some(TERMINAL_PEER_SDP_DEFAULT_MAX_MESSAGE_SIZE),
        Some(0) => None,
        Some(advertised) => Some(advertised),
    };
    if max_message_size.is_some_and(|size| size < TERMINAL_PEER_MAX_MESSAGE_SIZE as u64) {
        return Err(TerminalPeerSdpError::new(
            TerminalPeerSdpErrorCode::MaxMessageSize,
        ));
    }
    Ok(TerminalPeerSdpMetadata {
        fingerprint_sha256,
        candidate_count,
        candidate_types: TerminalPeerCandidateType::ORDER
            .into_iter()
            .filter(|kind| candidate_types.contains(kind))
            .collect(),
        max_message_size,
    })
}

fn effective<'a>(
    session_value: Option<&'a str>,
    application_value: Option<&'a str>,
    code: TerminalPeerSdpErrorCode,
) -> Result<&'a str, TerminalPeerSdpError> {
    application_value
        .or(session_value)
        .ok_or_else(|| TerminalPeerSdpError::new(code))
}

/// Removes browser-generated ICE-TCP candidates before the UDP-only offer
/// crosses a trust boundary.
pub fn filter_browser_terminal_peer_udp_candidates(sdp: &str) -> String {
    let mut lines = split_sdp_lines(sdp);
    if lines.last() == Some(&"") {
        lines.pop();
    }
    let kept: Vec<&str> = lines
        .into_iter()
        .filter(|line| !is_ice_tcp_candidate(line))
        .collect();
    let mut filtered = kept.join("\r\n");
    filtered.push_str("\r\n");
    filtered
}

fn is_ice_tcp_candidate(line: &str) -> bool {
    let Some(value) = line.strip_prefix(CANDIDATE_PREFIX) else {
        return false;
    };
    value
        .split(' ')
        .nth(2)
        .is_some_and(|protocol| protocol.eq_ignore_ascii_case("tcp"))
}

/// Normalizes an SDP SHA-256 fingerprint without keeping its source
/// formatting, so two spellings of one certificate compare equal.
pub fn normalize_terminal_peer_sha256_fingerprint(value: &str) -> Option<String> {
    if is_colon_form_sha256(value) {
        let normalized: String = value
            .chars()
            .filter(|character| *character != ':')
            .map(|character| character.to_ascii_lowercase())
            .collect();
        return Some(normalized);
    }
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Some(value.to_ascii_lowercase());
    }
    None
}

fn is_colon_form_sha256(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 95
        && bytes.iter().enumerate().all(|(index, byte)| {
            if index % 3 == 2 {
                *byte == b':'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}
