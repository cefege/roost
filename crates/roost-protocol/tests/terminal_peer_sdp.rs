//! Bounded SDP admission, before any native or browser ICE processing begins.
//!
//! The accepted fixture is data-channel-only and uses documentation and mDNS
//! candidates. The failures assert a fixed code and nothing else: a rejection
//! is logged, so a rejection that quoted the SDP would log its ICE password.
// A behaviour test unwraps the value it is asserting about: a failure
// there is the assertion failing, which is exactly what a test wants. The
// workspace denies `unwrap`/`expect` because a panic on a bad wire value in
// a running component is a fleet-visible outage, and that reasoning does not
// reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_protocol::terminal_peer::peer::{
    TERMINAL_PEER_MAX_MESSAGE_SIZE, TERMINAL_PEER_SDP_DEFAULT_MAX_MESSAGE_SIZE,
    TERMINAL_PEER_SDP_MAX_CANDIDATES, TERMINAL_PEER_SDP_MAX_LINE_UTF8_BYTES,
    TERMINAL_PEER_SDP_MAX_LINES, TERMINAL_PEER_SDP_MAX_UTF8_BYTES,
};
use roost_protocol::terminal_peer::sdp::{
    TerminalPeerCandidateType, TerminalPeerSdpErrorCode,
    filter_browser_terminal_peer_udp_candidates, inspect_terminal_peer_sdp,
    normalize_terminal_peer_sha256_fingerprint,
};

fn fingerprint(seed: u8) -> String {
    (0..32)
        .map(|index| format!("{:02x}", (index as u8).wrapping_add(seed)))
        .collect::<Vec<String>>()
        .join(":")
}

const ICE_PASSWORD: &str = "pppppppppppppppppppppp";
const HOST_CANDIDATE: &str = "a=candidate:host 1 udp 2122260223 192.0.2.8 5000 typ host";
const MDNS_CANDIDATE: &str = "a=candidate:mdns 1 udp 2122260222 browser-a.local 5001 typ host";
const SRFLX_CANDIDATE: &str =
    "a=candidate:srflx 1 udp 2122260221 198.51.100.8 5002 typ srflx raddr 0.0.0.0 rport 9";

fn valid_sdp(extra_application_lines: &[&str]) -> String {
    let mut lines = vec![
        "v=0".to_string(),
        "o=- 1 2 IN IP4 127.0.0.1".to_string(),
        "s=-".to_string(),
        "t=0 0".to_string(),
        "m=application 9 UDP/DTLS/SCTP webrtc-datachannel".to_string(),
        "a=setup:actpass".to_string(),
        format!("a=fingerprint:sha-256 {}", fingerprint(0)),
        "a=ice-ufrag:offer-ufrag".to_string(),
        format!("a=ice-pwd:{ICE_PASSWORD}"),
        format!("a=max-message-size:{TERMINAL_PEER_MAX_MESSAGE_SIZE}"),
        HOST_CANDIDATE.to_string(),
        MDNS_CANDIDATE.to_string(),
        SRFLX_CANDIDATE.to_string(),
    ];
    lines.extend(extra_application_lines.iter().map(|line| line.to_string()));
    lines.push(String::new());
    lines.join("\r\n")
}

fn code_of(sdp: &str) -> TerminalPeerSdpErrorCode {
    let error = inspect_terminal_peer_sdp(sdp).expect_err("this SDP is refused");
    let rendered = error.to_string();
    assert!(
        rendered.starts_with("terminal peer SDP rejected: "),
        "a rejection carries its code: {rendered}"
    );
    assert!(
        !rendered.contains("ice-pwd"),
        "a rejection never quotes the SDP: {rendered}"
    );
    error.code
}

#[test]
fn the_bounded_data_channel_contract_is_accepted() {
    let metadata = inspect_terminal_peer_sdp(&valid_sdp(&[])).expect("the fixture is admissible");
    assert_eq!(metadata.fingerprint_sha256, fingerprint(0).replace(':', ""));
    assert_eq!(metadata.candidate_count, 3);
    assert_eq!(
        metadata.candidate_types,
        vec![
            TerminalPeerCandidateType::Host,
            TerminalPeerCandidateType::Srflx
        ]
    );
    assert_eq!(
        metadata.max_message_size,
        Some(TERMINAL_PEER_MAX_MESSAGE_SIZE as u64)
    );
    assert_eq!(
        normalize_terminal_peer_sha256_fingerprint(&fingerprint(0).to_uppercase()),
        Some(fingerprint(0).replace(':', ""))
    );
    // A loopback address is inside the unicast range this contract admits.
    let loopback = valid_sdp(&[]).replace(
        HOST_CANDIDATE,
        "a=candidate:loopback 1 udp 2122260223 127.0.0.1 5000 typ host",
    );
    let metadata = inspect_terminal_peer_sdp(&loopback).expect("a host candidate is admissible");
    assert!(
        metadata
            .candidate_types
            .contains(&TerminalPeerCandidateType::Host)
    );
}

#[test]
fn ice_tcp_candidates_are_filtered_before_udp_only_signaling() {
    let tcp_candidate = "a=candidate:tcp 1 tcp 2122260223 192.0.2.8 9 typ host tcptype passive";
    let filtered = filter_browser_terminal_peer_udp_candidates(&valid_sdp(&[tcp_candidate]));
    assert!(!filtered.contains(tcp_candidate));
    assert!(filtered.ends_with("\r\n"));
    let metadata = inspect_terminal_peer_sdp(&filtered).expect("the filtered offer is admissible");
    assert_eq!(metadata.candidate_count, 3);
}

#[test]
fn a_second_media_section_and_a_conflicting_identity_are_refused() {
    assert_eq!(
        code_of(&valid_sdp(&["m=audio 9 UDP/TLS/RTP/SAVPF 111"])),
        TerminalPeerSdpErrorCode::Media
    );
    assert_eq!(
        code_of(&valid_sdp(&[&format!(
            "a=fingerprint:sha-256 {}",
            fingerprint(0xff)
        )])),
        TerminalPeerSdpErrorCode::Fingerprint
    );
    assert_eq!(
        code_of(&valid_sdp(&["a=setup:maybe"])),
        TerminalPeerSdpErrorCode::Media
    );
    assert_eq!(code_of("v=0\r\n"), TerminalPeerSdpErrorCode::Media);
}

#[test]
fn message_size_defaults_normalize_and_a_size_below_a_packet_is_refused() {
    let without_password = valid_sdp(&[]).replace(&format!("a=ice-pwd:{ICE_PASSWORD}\r\n"), "");
    assert_eq!(
        code_of(&without_password),
        TerminalPeerSdpErrorCode::IceCredentials
    );

    let implicit = valid_sdp(&[]).replace(
        &format!("a=max-message-size:{TERMINAL_PEER_MAX_MESSAGE_SIZE}\r\n"),
        "",
    );
    assert_eq!(
        inspect_terminal_peer_sdp(&implicit)
            .expect("an absent size is the default")
            .max_message_size,
        Some(TERMINAL_PEER_SDP_DEFAULT_MAX_MESSAGE_SIZE)
    );

    let unbounded = valid_sdp(&[]).replace(
        &format!("a=max-message-size:{TERMINAL_PEER_MAX_MESSAGE_SIZE}"),
        "a=max-message-size:0",
    );
    assert_eq!(
        inspect_terminal_peer_sdp(&unbounded)
            .expect("an explicit zero is unlimited, not missing")
            .max_message_size,
        None
    );

    let too_small = valid_sdp(&[]).replace(
        &format!("a=max-message-size:{TERMINAL_PEER_MAX_MESSAGE_SIZE}"),
        &format!("a=max-message-size:{}", TERMINAL_PEER_MAX_MESSAGE_SIZE - 1),
    );
    assert_eq!(
        code_of(&too_small),
        TerminalPeerSdpErrorCode::MaxMessageSize
    );
}

#[test]
fn candidates_that_could_change_the_authorized_boundary_are_refused() {
    for candidate in [
        "a=candidate:tcp 1 tcp 2122260223 192.0.2.8 5000 typ host",
        "a=candidate:component 2 udp 2122260223 192.0.2.8 5000 typ host",
        "a=candidate:relay 1 udp 2122260223 192.0.2.8 5000 typ relay",
        "a=candidate:hostname 1 udp 2122260223 arbitrary.example 5000 typ host",
        "a=candidate:unspecified 1 udp 2122260223 0.0.0.0 5000 typ host",
        "a=candidate:multicast 1 udp 2122260223 224.0.0.1 5000 typ host",
        "a=candidate:short 1 udp 2122260223 192.0.2.8 5000 typ",
        "a=candidate:tcpext 1 udp 2122260223 192.0.2.8 5000 typ host tcptype passive",
        "a=candidate:related 1 udp 2122260223 192.0.2.8 5000 typ host raddr nonsense rport 9",
    ] {
        assert_eq!(
            code_of(&valid_sdp(&[candidate])),
            TerminalPeerSdpErrorCode::Candidate,
            "{candidate}"
        );
    }
    // A candidate before the media section names an address the offer never
    // authorized, so it is refused rather than collected.
    let hoisted = format!("{SRFLX_CANDIDATE}\r\n{}", valid_sdp(&[]));
    assert_eq!(code_of(&hoisted), TerminalPeerSdpErrorCode::Candidate);
}

#[test]
fn resource_bounds_are_refused_before_negotiation() {
    let oversized = "x".repeat(TERMINAL_PEER_SDP_MAX_UTF8_BYTES + 1);
    assert_eq!(code_of(&oversized), TerminalPeerSdpErrorCode::SdpSize);
    let long_line = format!(
        "{}\r\n",
        "a=x"
            .to_string()
            .repeat(TERMINAL_PEER_SDP_MAX_LINE_UTF8_BYTES)
    );
    assert_eq!(code_of(&long_line), TerminalPeerSdpErrorCode::SdpLine);
    let many_lines = std::iter::repeat_n("a=x", TERMINAL_PEER_SDP_MAX_LINES + 1)
        .collect::<Vec<&str>>()
        .join("\r\n");
    assert_eq!(code_of(&many_lines), TerminalPeerSdpErrorCode::SdpLines);
    let flood: Vec<&str> =
        std::iter::repeat_n(HOST_CANDIDATE, TERMINAL_PEER_SDP_MAX_CANDIDATES).collect();
    assert_eq!(
        code_of(&valid_sdp(&flood)),
        TerminalPeerSdpErrorCode::CandidateCount
    );
}

#[test]
fn a_line_that_is_not_printable_ascii_is_refused() {
    assert_eq!(code_of("\u{0}a=x"), TerminalPeerSdpErrorCode::SdpLine);
    assert_eq!(code_of("A=x"), TerminalPeerSdpErrorCode::SdpLine);
    assert_eq!(code_of("plain"), TerminalPeerSdpErrorCode::SdpLine);
}
