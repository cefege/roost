//! The direct peer's own lifecycle through the state machine: the
//! loopback-before-peer election, the offer/answer exchange, the grant it
//! authenticates with, and the bounds that refuse an attempt. Each case is a
//! rule the machine holds with no transport in the process; the four fault
//! rules are in `terminal_peer_fallback.rs`.

use roost_client_core::{Effect, SyncCommand, TerminalTransport};
use roost_client_core::client::carriers::{
    CarrierEffect, CarrierEnvironment, CarrierFault, DirectGrant, GrantInput, GrantPhase,
    GRANT_RETRY_MS, LOOPBACK_GRACE_MS, LoopbackProbe, PeerAnswer, PeerPhase, PeerSignalling,
    ReadyTuple, ScriptedPeerSignalling, Signalling, SignallingInput,
};

const WORKER: &str = "worker-a";
const SESSION: &str = "session-a";
const EPOCH: &str = "epoch-a";
const SYNC_GENERATION: u64 = 7;
const NOW: u64 = 1_000;
const GRANT_TTL_MS: u64 = 60 * 60_000;

/// The first peer id the machine mints, which is `peer-{attempt_id}`.
const FIRST_PEER_ID: &str = "peer-1";

/// A SHA-256 SDP with one UDP candidate: the exact shape v2's fake browser
/// produced (`terminalPeerConnection.test.ts:31-45`), so the machine's
/// readability check is exercised against an SDP the real inspector accepts
/// rather than one written to suit it.
fn usable_sdp() -> String {
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

fn grant_for(sessions: &[&str]) -> DirectGrant {
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

fn machine(peers_allocated: u32) -> Signalling {
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

fn demand(session: &str) -> SignallingInput {
    SignallingInput::Demand {
        session_id: session.to_string(),
        view_id: format!("view-{session}"),
        active: true,
    }
}

fn elsewhere() -> SignallingInput {
    SignallingInput::LocalDoorAnswered {
        worker_fp: String::new(),
    }
}

fn grant_minted(sessions: &[&str]) -> SignallingInput {
    SignallingInput::Grant(GrantInput::Minted(grant_for(sessions)))
}

fn ready(worker_epoch: &str, sessions: &[&str]) -> ReadyTuple {
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
fn authenticating() -> (Signalling, u64) {
    let mut peer = machine(0);
    peer.step(demand(SESSION));
    peer.step(grant_minted(&[SESSION]));
    peer.step(elsewhere());
    let attempt_id = match peer.step(SignallingInput::OfferReady {
        attempt_id: 1,
        offer_sdp: usable_sdp(),
    })
    .as_slice()
    {
        [CarrierEffect::NegotiateOffer { attempt_id, .. }] => attempt_id,
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

fn opened_a_transport(effects: &[CarrierEffect]) -> bool {
    effects
        .iter()
        .any(|effect| matches!(effect, CarrierEffect::OpenTransport { .. }))
}

fn faults(effects: &[CarrierEffect]) -> Vec<CarrierFault> {
    let named = |effect: &CarrierEffect| match effect {
        CarrierEffect::Faulted { fault, .. } => Some(*fault),
        _ => None,
    };
    effects.iter().filter_map(named).collect()
}

#[test]
fn loopback_wins_before_a_webrtc_peer_is_allocated_and_keeps_sync_metadata_live() {
    let mut peer = machine(0);
    let mut emitted: Vec<CarrierEffect> = Vec::new();
    // A view opens a pane on the worker this page is served by.
    emitted.extend(peer.step(demand(SESSION)));
    emitted.extend(peer.step(grant_minted(&[SESSION])));

    // The probe answers SAME HOST. From here a peer must never be allocated,
    // however long the machine is asked to wait.
    emitted.extend(peer.step(SignallingInput::LocalDoorAnswered {
        worker_fp: WORKER.to_string(),
    }));
    emitted.extend(peer.step(SignallingInput::LoopbackCarrierStaged {
        staged: true,
    }));
    emitted.extend(peer.step(SignallingInput::RetryDue {
        now_ms: NOW + LOOPBACK_GRACE_MS,
    }));
    emitted.extend(peer.step(SignallingInput::Sweep { now_ms: NOW + 1_000 }));

    assert!(!opened_a_transport(&emitted), "loopback holds, so no peer; {emitted:?}");
    assert!(
        !peer.loopback_probe().permits_peer() && peer.loopback_probe().has_staged_carrier(),
        "the probe withholds a peer, and sees the fast path that holds it"
    );
    let snapshot = peer.snapshot();
    assert_eq!(snapshot.transport_held, None, "no peer was elected");
    assert_eq!(snapshot.fallback_reason, None, "nothing failed");

    // "Keeps Sync metadata live" is a claim about what the machine DID NOT do: a
    // redial, a link close, or a domain re-subscribe would each drop the
    // session's metadata authority, and the worker would then have to serve a
    // session the client had stopped listening for.
    let touches_sync = |core: &Effect| {
        matches!(
            core,
            Effect::DialSync { .. }
                | Effect::CloseSyncLink { .. }
                | Effect::SendSync(SyncCommand::DomainReady { .. })
                | Effect::SendSync(SyncCommand::Unsubscribe { .. })
        )
    };
    for effect in &emitted {
        let CarrierEffect::Core(core) = effect else {
            continue;
        };
        assert!(
            !touches_sync(core),
            "a direct-carrier decision may never touch Sync authority; got {core:?}"
        );
    }
    let generation = peer.snapshot().sync_generation;
    assert_eq!(generation, SYNC_GENERATION, "the Sync fence must not move");
}

#[test]
fn retries_a_transient_initial_grant_failure_at_the_bounded_retry_deadline() {
    let mut peer = machine(0);
    let exact = CarrierEffect::Core(Effect::RequestDirectGrant {
        session_id: SESSION.to_string(),
        worker_fp: WORKER.to_string(),
    });
    let requested = peer.step(demand(SESSION));
    assert!(requested.contains(&exact), "new demand asks for a grant; {requested:?}");

    // A request that RETURNS is a refusal, not a pending state: only a worker's
    // acknowledgement reveals the secret.
    let refused = peer.step(SignallingInput::Grant(GrantInput::Refused {
        now_ms: NOW,
        reason: "worker did not acknowledge".to_string(),
    }));
    assert_eq!(
        peer.snapshot().grant_phase,
        GrantPhase::Unavailable,
        "a returned request is a refusal, not a pending state"
    );
    let armed = refused.iter().find_map(|effect| match effect {
        CarrierEffect::RetryAt { at_ms } => Some(*at_ms),
        _ => None,
    });
    let armed = armed.unwrap_or_else(|| panic!("a refusal arms a retry; {refused:?}"));
    assert_eq!(armed, NOW + GRANT_RETRY_MS);
    let asked = |effect: &CarrierEffect| {
        matches!(effect, CarrierEffect::Core(Effect::RequestDirectGrant { .. }))
    };
    let early = peer.step(SignallingInput::RetryDue { now_ms: armed - 1 });
    assert!(!early.iter().any(asked), "the retry must not fire early");
    let retried = peer.step(SignallingInput::RetryDue { now_ms: armed });
    assert_eq!(
        retried.iter().filter(|effect| asked(effect)).count(),
        1,
        "the retry asks once, not once per demand; got {retried:?}"
    );
}

#[test]
fn a_carrier_with_no_live_grant_never_authenticates() {
    let mut peer = machine(0);
    peer.step(demand(SESSION));
    peer.step(elsewhere());
    let effects = peer.step(SignallingInput::OfferReady {
        attempt_id: 1,
        offer_sdp: usable_sdp(),
    });
    assert!(!opened_a_transport(&effects), "no grant, no transport; {effects:?}");
    let snapshot = peer.snapshot();
    assert_eq!(snapshot.grant_phase, GrantPhase::Requested);
    assert_eq!(snapshot.phase, PeerPhase::AwaitingGrant);
}

#[test]
fn a_matching_answer_and_ready_stage_a_carrier_that_is_not_yet_elected() {
    let (mut peer, attempt_id) = authenticating();
    let staged = peer.step(SignallingInput::PeerAuthenticated {
        attempt_id,
        ready: ready(EPOCH, &[SESSION]),
    });
    assert!(faults(&staged).is_empty(), "a matching tuple is not a fault");
    assert!(
        staged.iter().any(|effect| matches!(effect, CarrierEffect::StageCarrier { .. })),
        "a matching Ready is what stages a candidate; got {staged:?}"
    );
    let snapshot = peer.snapshot();
    assert_eq!(snapshot.phase, PeerPhase::Candidate);
    assert!(snapshot.has_carrier, "held, not yet elected");
    assert_eq!(snapshot.transport_held, Some(TerminalTransport::Peer));
}

#[test]
fn rejects_a_ready_tuple_with_a_different_worker_epoch() {
    let (mut peer, attempt_id) = authenticating();
    let effects = peer.step(SignallingInput::PeerAuthenticated {
        attempt_id,
        ready: ready("epoch-b", &[SESSION]),
    });
    assert_eq!(faults(&effects), vec![CarrierFault::IdentityMismatch]);
    let staged = |effect: &CarrierEffect| matches!(effect, CarrierEffect::StageCarrier { .. });
    assert!(!effects.iter().any(staged), "a refused tuple stages nothing");
    assert!(!peer.snapshot().has_carrier, "nothing is held after a refusal");
}

#[test]
fn accepts_a_bounded_ready_for_the_full_256_session_grant() {
    let sessions: Vec<String> = (0..256)
        .map(|index| format!("00000000-0000-4000-8000-{index:012}"))
        .collect();
    let borrowed: Vec<&str> = sessions.iter().map(String::as_str).collect();
    let mut peer = machine(0);
    for session in &sessions {
        peer.step(demand(session));
    }
    peer.step(grant_minted(&borrowed));
    peer.step(elsewhere());
    let offer = peer.step(SignallingInput::OfferReady {
        attempt_id: 1,
        offer_sdp: usable_sdp(),
    });
    assert!(
        opened_a_transport(&offer),
        "a 256-session grant is exactly in bounds, not over it"
    );
    let widest = ReadyTuple {
        worker_fp: WORKER.to_string(),
        worker_epoch: EPOCH.to_string(),
        peer_id: FIRST_PEER_ID.to_string(),
        socket_generation: 8,
        socket_id: "socket-max-scope".to_string(),
        session_ids: sessions.iter().cloned().collect(),
    };
    let staged = peer.step(SignallingInput::PeerAuthenticated {
        attempt_id: 1,
        ready: widest,
    });
    let admitted = staged.iter().any(|effect| matches!(effect, CarrierEffect::StageCarrier { .. }));
    assert!(admitted, "a Ready inside the grant's scope is admitted");
}

#[test]
fn refuses_a_ninth_simultaneously_demanded_browser_peer() {
    let mut opened = Vec::new();
    let mut refused = Vec::new();
    for index in 0..9u32 {
        let session = format!("session-{index}");
        let mut peer = machine(index);
        peer.step(demand(&session));
        peer.step(SignallingInput::Grant(GrantInput::Minted(DirectGrant {
            worker_fp: format!("worker-{index}"),
            ..grant_for(&[&session])
        })));
        let mut effects = peer.step(elsewhere());
        effects.extend(peer.step(SignallingInput::OfferReady {
            attempt_id: 1,
            offer_sdp: usable_sdp(),
        }));
        opened.push(opened_a_transport(&effects));
        refused.push(faults(&effects));
    }
    assert!(
        opened[..8].iter().all(|entry| *entry) && !opened[8],
        "eight peers fit the document-wide cap and the ninth does not; got {opened:?}"
    );
    assert_eq!(refused[8], vec![CarrierFault::DocumentCap], "a cap, not a fault");
}

#[test]
fn the_probe_releases_a_peer_only_for_a_page_that_is_not_on_the_worker_machine() {
    let mut probe = LoopbackProbe::new(WORKER);
    assert!(!probe.permits_peer(), "an unanswered probe releases no peer");
    probe.answered("worker-b");
    assert!(probe.permits_peer(), "another worker's page cannot use this door");
    let mut same = LoopbackProbe::new(WORKER);
    same.answered(WORKER);
    assert!(!same.permits_peer(), "the worker's own page must not peer");
    assert_eq!(same.recheck_after_ms(), None, "a settled answer is not re-asked");
}

#[test]
fn the_machine_is_reachable_through_its_named_trait() {
    // The trait is the seam this slice is designed around: a host holds a `dyn
    // PeerSignalling`, never the concrete machine, so a different WebRTC stack
    // — or no WebRTC stack at all — changes one file rather than every caller.
    let mut peer: Box<dyn PeerSignalling> = Box::new(machine(0));
    assert_eq!(peer.worker_fp(), WORKER);
    let effects = peer.step(SignallingInput::Demand {
        session_id: SESSION.to_string(),
        view_id: "view-a".to_string(),
        active: true,
    });
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, CarrierEffect::Core(Effect::RequestDirectGrant { .. }))),
        "the trait must reach the decision the concrete type makes; got {effects:?}"
    );
    assert_eq!(peer.snapshot().active_views, 1);
    assert_eq!(
        peer.phase(),
        PeerPhase::Idle,
        "with no probe answer and no grant, the machine is parked, not negotiating"
    );

    let mut scripted = ScriptedPeerSignalling::new(WORKER);
    scripted.script(vec![CarrierEffect::RetryAt { at_ms: 42 }]);
    let doubled: &mut dyn PeerSignalling = &mut scripted;
    let scripted_out = doubled.step(SignallingInput::WorkerRetired);
    assert_eq!(
        scripted_out,
        vec![CarrierEffect::RetryAt { at_ms: 42 }],
        "the double must be substitutable for the machine behind the same trait"
    );
    assert_eq!(scripted.observed_inputs(), &[SignallingInput::WorkerRetired]);
}
