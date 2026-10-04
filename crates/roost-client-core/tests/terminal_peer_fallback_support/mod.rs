//! The described machine two peer-fallback binaries share: one live grant, one
//! page on another machine, and the SDPs and tuples a negotiation is built from.
//!
//! The credential rules and the handover rules ask the same machine the same
//! questions — what the fault did to the grant, and where the session went — so
//! the described environment is defined once here rather than twice with the
//! two copies drifting. The session-authority oracle lives here for the same
//! reason: it is one property, and both binaries must read it the same way.

#![allow(dead_code)]

use roost_client_core::client::carriers::{
    CarrierEffect, CarrierEnvironment, CarrierFault, DirectGrant, FaultFallback, GrantInput,
    PeerAnswer, ReadyTuple, Signalling, SignallingInput,
};
use roost_client_core::{Effect, SyncCommand};

pub const WORKER: &str = "worker-a";
pub const SESSION: &str = "session-a";
pub const EPOCH: &str = "epoch-a";
pub const SYNC_GENERATION: u64 = 7;
pub const NOW: u64 = 1_000;
pub const GRANT_TTL_MS: u64 = 60 * 60_000;
pub const FIRST_PEER_ID: &str = "6f9d2c1a-3b4e-4c5d-8e7f-0a1b2c3d4e5f";

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

/// An offer the worker cannot read: exactly what the smoke fault `invalid_sdp`
/// injects by replacing the SDP before the worker ever sees it.
pub fn unreadable_sdp() -> String {
    "smoke-invalid-sdp".to_string()
}

pub fn grant() -> DirectGrant {
    DirectGrant {
        grant_id: "grant-a".to_string(),
        secret: "secret-a".to_string(),
        worker_fp: WORKER.to_string(),
        worker_epoch: EPOCH.to_string(),
        tab_id: "tab-a".to_string(),
        device_fingerprint: "device-a".to_string(),
        session_ids: [SESSION.to_string()].into_iter().collect(),
        peer_supported: true,
        input_route_supported: true,
        stun_urls: Vec::new(),
        expires_at_ms: NOW + GRANT_TTL_MS,
    }
}

pub fn machine() -> Signalling {
    Signalling::new(
        WORKER,
        CarrierEnvironment {
            peers_allocated: 0,
            peer_transport_available: true,
            sync_generation: SYNC_GENERATION,
        },
        NOW,
    )
}

pub fn demand() -> SignallingInput {
    SignallingInput::Demand {
        session_id: SESSION.to_string(),
        view_id: "view-a".to_string(),
        active: true,
        now_ms: NOW,
    }
}

pub fn answer_for(attempt_id: u64) -> SignallingInput {
    SignallingInput::AnswerReceived {
        attempt_id,
        answer: PeerAnswer {
            peer_id: FIRST_PEER_ID.to_string(),
            worker_epoch: EPOCH.to_string(),
            answer_sdp: usable_sdp(),
        },
    }
}

pub fn ready_with(worker_epoch: &str) -> ReadyTuple {
    ReadyTuple {
        worker_fp: WORKER.to_string(),
        worker_epoch: worker_epoch.to_string(),
        peer_id: FIRST_PEER_ID.to_string(),
        socket_generation: 7,
        socket_id: "socket-a".to_string(),
        session_ids: [SESSION.to_string()].into_iter().collect(),
    }
}

/// A machine whose peer is open, with the grant live and the page on a machine
/// that is not the worker's own. Returns the live attempt id.
pub fn peer_open() -> (Signalling, u64) {
    let mut peer = machine();
    peer.step(demand());
    peer.step(SignallingInput::Grant(GrantInput::Minted(grant())));
    peer.step(SignallingInput::LocalDoorAnswered {
        worker_fp: String::new(),
    });
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
    (peer, attempt_id)
}

/// A machine whose peer has answered and is authenticating.
pub fn authenticating() -> (Signalling, u64) {
    let (mut peer, attempt_id) = peer_open();
    peer.step(answer_for(attempt_id));
    (peer, attempt_id)
}

/// Whether any effect in `effects` would make the worker look like it had lost
/// the session.
///
/// THE property `smoke/terminal/terminal-peer.spec.ts:263` exists for. A PTY
/// lives in the worker and no client can recreate it, but a client that re-dials
/// Sync, closes the Sync link, or re-subscribes a domain drops the session's
/// metadata authority, and the worker's next frame then has nowhere to land. So
/// "the PTY is not recreated" is observable here as the absence of exactly those
/// three, and a fault that emitted one would fail this.
pub fn session_authority_teardowns(effects: &[CarrierEffect]) -> Vec<&'static str> {
    let mut torn = Vec::new();
    for effect in effects {
        let CarrierEffect::Core(core) = effect else {
            continue;
        };
        match core {
            Effect::DialSync { .. } => torn.push("dial_sync"),
            Effect::CloseSyncLink { .. } => torn.push("close_sync_link"),
            Effect::SendSync(SyncCommand::DomainReady { .. }) => torn.push("domain_ready"),
            Effect::SendSync(SyncCommand::Unsubscribe { .. }) => torn.push("unsubscribe"),
            _ => {}
        }
    }
    torn
}

pub fn faults(effects: &[CarrierEffect]) -> Vec<CarrierFault> {
    let named = |effect: &CarrierEffect| match effect {
        CarrierEffect::Faulted { fault, .. } => Some(*fault),
        _ => None,
    };
    effects.iter().filter_map(named).collect()
}

pub fn handovers(effects: &[CarrierEffect]) -> Vec<FaultFallback> {
    let named = |effect: &CarrierEffect| match effect {
        CarrierEffect::Fallback { transport, .. } => Some(*transport),
        _ => None,
    };
    effects.iter().filter_map(named).collect()
}

/// The session keeps its Sync authority. Named so a failure says which half of
/// the property broke.
pub fn assert_no_session_teardown(effects: &[CarrierEffect]) {
    assert_eq!(
        session_authority_teardowns(effects),
        Vec::<&str>::new(),
        "a direct-carrier fault must not re-open the session; got {effects:?}"
    );
}

/// The shared assertion: the session keeps its Sync authority and, with no
/// loopback carrier staged, is handed to Sync.
pub fn assert_fell_back_without_reopening_the_session(effects: &[CarrierEffect]) {
    assert_no_session_teardown(effects);
    assert_eq!(
        handovers(effects),
        vec![FaultFallback::Sync],
        "with no loopback carrier staged, the session is served by Sync; got {effects:?}"
    );
}
