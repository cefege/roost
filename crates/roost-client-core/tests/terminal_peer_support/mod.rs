//! The environment the direct peer's tests all describe: one worker, one
//! session, one epoch, the SDP the real browser produces, and a machine already
//! advanced to the point where its peer is authenticating.
//!
//! Every binary in the `terminal_peer_*` family asks a different question about
//! the same machine, so the described environment is defined once here rather
//! than four times with the copies drifting. Depends on
//! `roost_client_core::client::carriers` and nothing else.
#![allow(dead_code)]

use roost_client_core::client::carriers::{
    CarrierEffect, CarrierEnvironment, CarrierFault, DirectGrant, GrantInput, PeerAnswer,
    ReadyTuple, Signalling, SignallingInput,
};

pub const WORKER: &str = "worker-a";
pub const SESSION: &str = "session-a";
pub const EPOCH: &str = "epoch-a";
pub const SYNC_GENERATION: u64 = 7;
pub const NOW: u64 = 1_000;
pub const GRANT_TTL_MS: u64 = 60 * 60_000;

/// The peer id the host mints for the first attempt and reports with its offer.
/// A UUID, because the coordinator refuses any other shape.
pub const FIRST_PEER_ID: &str = "6f9d2c1a-3b4e-4c5d-8e7f-0a1b2c3d4e5f";

/// A SHA-256 SDP with one UDP candidate: the exact shape v2's fake browser
/// produced (`terminalPeerConnection.test.ts:31-45`), so the machine's
/// readability check is exercised against an SDP the real inspector accepts
/// rather than one written to suit it.
pub fn usable_sdp() -> String {
    let fingerprint = vec!["AA"; 32].join(":");
    [
        "v=0",
        "o=- 1 1 IN IP4 127.0.0.1",
        "s=-",
        "t=0 0",
        "a=ice-ufrag:test",
        "a=ice-pwd:0123456789012345678901",
        &format!("a=fingerprint:sha-256 {fingerprint}"),
        "m=application 9 UDP/DTLS/SCTP webrtc-datachannel",
        "a=setup:actpass",
        "a=sctp-port:5000",
        "a=max-message-size:65536",
        "a=candidate:1 1 udp 1 192.0.2.1 5000 typ host",
        "",
    ]
    .join("\r\n")
}

pub fn grant_for(sessions: &[&str]) -> DirectGrant {
    let scope = || sessions.iter().map(|id| id.to_string()).collect();
    DirectGrant {
        grant_id: "grant-a".to_string(),
        secret: "secret-a".to_string(),
        worker_fp: WORKER.to_string(),
        worker_epoch: EPOCH.to_string(),
        tab_id: "tab-a".to_string(),
        device_fingerprint: "device-a".to_string(),
        session_ids: scope(),
        peer_supported: true,
        input_route_supported: true,
        stun_urls: Vec::new(),
        expires_at_ms: NOW + GRANT_TTL_MS,
    }
}

pub fn machine(peers_allocated: u32) -> Signalling {
    Signalling::new(
        WORKER,
        CarrierEnvironment {
            peers_allocated,
            peer_transport_available: true,
            sync_generation: SYNC_GENERATION,
        },
        NOW,
    )
}

pub fn demand(session: &str) -> SignallingInput {
    SignallingInput::Demand {
        session_id: session.to_string(),
        view_id: format!("view-{session}"),
        active: true,
        now_ms: NOW,
    }
}

pub fn elsewhere() -> SignallingInput {
    SignallingInput::LocalDoorAnswered {
        worker_fp: String::new(),
    }
}

pub fn grant_minted(sessions: &[&str]) -> SignallingInput {
    SignallingInput::Grant(GrantInput::Minted(grant_for(sessions)))
}

pub fn ready(worker_epoch: &str, sessions: &[&str]) -> ReadyTuple {
    ReadyTuple {
        worker_fp: WORKER.to_string(),
        worker_epoch: worker_epoch.to_string(),
        peer_id: FIRST_PEER_ID.to_string(),
        socket_generation: 7,
        socket_id: "socket-a".to_string(),
        session_ids: sessions.iter().map(|id| id.to_string()).collect(),
    }
}

/// Take a machine to the point where its peer is authenticating: demand, grant,
/// a probe answer that puts the page on another machine, an offer, and an
/// answer. Returns the live attempt id.
pub fn authenticating() -> (Signalling, u64) {
    let mut peer = machine(0);
    peer.step(demand(SESSION));
    peer.step(grant_minted(&[SESSION]));
    peer.step(elsewhere());
    let attempt_id = match peer
        .step(SignallingInput::OfferReady {
            attempt_id: 1,
            peer_id: FIRST_PEER_ID.to_string(),
            offer_sdp: usable_sdp(),
        })
        .as_slice()
    {
        [CarrierEffect::NegotiateOffer { attempt_id, .. }] => *attempt_id,
        other => panic!("a page on another machine must negotiate a peer, got {other:?}"),
    };
    peer.step(SignallingInput::AnswerReceived {
        attempt_id,
        answer: PeerAnswer {
            peer_id: FIRST_PEER_ID.to_string(),
            worker_epoch: EPOCH.to_string(),
            answer_sdp: usable_sdp(),
        },
    });
    (peer, attempt_id)
}

pub fn opened_a_transport(effects: &[CarrierEffect]) -> bool {
    effects
        .iter()
        .any(|effect| matches!(effect, CarrierEffect::OpenTransport { .. }))
}

pub fn faults(effects: &[CarrierEffect]) -> Vec<CarrierFault> {
    let named = |effect: &CarrierEffect| match effect {
        CarrierEffect::Faulted { fault, .. } => Some(*fault),
        _ => None,
    };
    effects.iter().filter_map(named).collect()
}
