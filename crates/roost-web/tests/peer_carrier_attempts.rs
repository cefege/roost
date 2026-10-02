//! The WebRTC carrier's own rules, decided without a browser: the tuple a
//! `Ready` has to prove, the lane bytes a carrier frames and reassembles, the
//! attempt a replayed frame can still reach, and the generation a promotion
//! leaves behind.
//!
//! The browser is the ONE thing these cannot decide — an `RTCPeerConnection` is
//! not constructible in a native test binary — so what is proved here is
//! everything the browser's objects sit under: `platform::carriers::PeerCarrier`
//! and `platform::carriers::PeerCarriers`. Each test below is a place where a
//! wrong answer produces a carrier that carries nothing, or carries something
//! that was never this attempt's.
//!
//! Test root, so the unwrap allowance is declared here (`CLAUDE.md` "the test
//! exemption reaches a test binary and not a fixture").

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};

use roost_client_core::TerminalToken;
use roost_client_core::TerminalTransport;
use roost_client_core::client::carriers::{PeerAttempt, PeerLane, ReadyTuple};
use roost_client_core::{
    DirectCarrier, PromotionCandidate, PromotionRefusal, RouteRegistry, TerminalSession,
};
use roost_web::platform::carriers::{PeerCarrier, PeerCarriers};

fn attempt(attempt_id: u64, worker_fp: &str, sessions: &[&str]) -> PeerAttempt {
    PeerAttempt {
        attempt_id,
        worker_fp: worker_fp.to_owned(),
        worker_epoch: "epoch-a".to_owned(),
        transport: TerminalTransport::Peer,
        peer_id: format!("peer-{attempt_id}"),
        grant_id: "grant-a".to_owned(),
        tab_id: "tab-a".to_owned(),
        device_fingerprint: "device-a".to_owned(),
        stun_urls: Vec::new(),
        session_ids: BTreeSet::from_iter(sessions.iter().map(|session| (*session).to_owned())),
    }
}

fn ready(attempt: &PeerAttempt, sessions: &[&str], socket_generation: u64) -> ReadyTuple {
    ReadyTuple {
        worker_fp: attempt.worker_fp.clone(),
        worker_epoch: attempt.worker_epoch.clone(),
        peer_id: attempt.peer_id.clone(),
        socket_generation,
        socket_id: format!("socket-{socket_generation}"),
        session_ids: BTreeSet::from_iter(sessions.iter().map(|session| (*session).to_owned())),
    }
}

/// One attempt opened in `carriers`, with its `Ready` admitted when it matches.
fn staged(
    carriers: &mut PeerCarriers,
    attempt: &PeerAttempt,
    sessions: &[&str],
    socket_generation: u64,
    connection_id: &str,
) -> roost_client_core::DirectCarrier {
    carriers.open(PeerCarrier::opened(attempt.clone(), 0));
    carriers
        .authenticate(
            attempt.attempt_id,
            &ready(attempt, sessions, socket_generation),
            connection_id.to_owned(),
        )
        .map(|(admitted, _)| admitted)
        .expect("a matching Ready admits")
}

/// A `Ready` that proves a DIFFERENT worker, epoch, peer, generation or scope is
/// refused, and no carrier is built from it.
///
/// The tuple is the only thing between an open peer and a registered route, so
/// every half of it is checked here rather than only the scope: a host that
/// admitted on the session set alone would register a generation minted from a
/// message that was never this negotiation's.
#[test]
fn a_ready_that_does_not_match_the_grant_is_refused() {
    let attempt = attempt(1, "worker-a", &["session-a"]);

    for (named, mismatched) in [
        (
            "another worker",
            ReadyTuple {
                worker_fp: "worker-b".to_owned(),
                ..ready(&attempt, &["session-a"], 7)
            },
        ),
        (
            "another worker process",
            ReadyTuple {
                worker_epoch: "epoch-b".to_owned(),
                ..ready(&attempt, &["session-a"], 7)
            },
        ),
        (
            "another negotiation",
            ReadyTuple {
                peer_id: "peer-9".to_owned(),
                ..ready(&attempt, &["session-a"], 7)
            },
        ),
        (
            "no socket generation to fence against",
            ready(&attempt, &["session-a"], 0),
        ),
        (
            "a wider scope than the grant names",
            ready(&attempt, &["session-a", "session-b"], 7),
        ),
    ] {
        let mut carrier = PeerCarrier::opened(attempt.clone(), 0);
        assert_eq!(
            carrier.authenticate(&mismatched, "conn-1".to_owned()),
            None,
            "a Ready claiming {named} must not admit a carrier"
        );
        assert!(
            !carrier.is_authenticated(),
            "a refused Ready leaves the attempt presenting nothing"
        );
    }
}

/// The matching `Ready` admits, and the carrier carries exactly what it proved.
#[test]
fn a_matching_ready_admits_a_carrier_carrying_exactly_its_own_scope() {
    let attempt = attempt(1, "worker-a", &["session-a"]);
    let mut carriers = PeerCarriers::new();
    carriers.open(PeerCarrier::opened(attempt.clone(), 0));
    let (admitted, displaced) = carriers
        .authenticate(1, &ready(&attempt, &["session-a"], 7), "conn-1".to_owned())
        .expect("a matching Ready admits");

    assert!(displaced.is_none());
    assert_eq!(admitted.connection_id, "conn-1");
    assert_eq!(admitted.worker_fp, "worker-a");
    assert_eq!(admitted.transport, TerminalTransport::Peer);
    assert_eq!(
        admitted.token,
        TerminalToken::direct(7, TerminalTransport::Peer, "worker-a", "epoch-a", 7),
        "the generation is the Ready's, so a worker restart fences the route"
    );
    assert!(admitted.allows_session("session-a"));
    assert!(
        !admitted.allows_session("session-b"),
        "a scope the Ready did not prove is not carried, however open the peer is"
    );
}

/// A message larger than one packet goes out as fragments and comes back whole.
///
/// The round trip is the property, not the byte count: a carrier that framed
/// every message into one packet, or that dropped the tail of a large one, would
/// paint a half-written baseline and look healthy doing it.
#[test]
fn a_queued_message_round_trips_across_the_packets_it_was_split_into() {
    let attempt = attempt(1, "worker-a", &["session-a"]);
    let tuple = ready(&attempt, &["session-a"], 7);
    let mut sender = PeerCarrier::opened(attempt.clone(), 0);
    let mut receiver = PeerCarrier::opened(attempt, 0);
    sender.authenticate(&tuple, "conn-1".to_owned()).unwrap();
    receiver.authenticate(&tuple, "conn-2".to_owned()).unwrap();

    let message: Vec<u8> = (0..40_000u32).map(|index| index as u8).collect();
    assert!(
        sender.enqueue(PeerLane::Data, message.clone()).unwrap(),
        "the data lane accepts a whole frame"
    );

    let mut packets = 0usize;
    let mut reassembled = Vec::new();
    while let Some(packet) = sender.next_packet(PeerLane::Data).unwrap() {
        packets += 1;
        if let Some(whole) = receiver.push(PeerLane::Data, 0, &packet).unwrap() {
            reassembled = whole;
        }
    }
    assert!(
        packets > 1,
        "a message this size cannot fit in one packet, so a single packet here \
         would mean the lane stopped splitting"
    );
    assert_eq!(
        reassembled, message,
        "the bytes the receiver reassembles are the bytes the sender framed"
    );
    assert_eq!(
        sender.queued_bytes(PeerLane::Data),
        0,
        "a committed fragment leaves the queue, so a redrain cannot duplicate it"
    );
}

/// A frame from an attempt this document no longer holds reaches no replica, and
/// the retire is that attempt's alone.
///
/// The two halves are one property: a shared table would let one worker's fault
/// retire another's carrier, and a table keyed by anything but the attempt id
/// would let a late frame land on whatever replaced it.
#[test]
fn a_frame_from_a_retired_attempt_reaches_nothing_and_retires_only_that_attempt() {
    let first = attempt(1, "worker-a", &["session-a"]);
    let second = attempt(2, "worker-b", &["session-a"]);
    let mut carriers = PeerCarriers::new();
    staged(&mut carriers, &first, &["session-a"], 7, "conn-a");
    staged(&mut carriers, &second, &["session-a"], 8, "conn-b");
    let first_token = carriers.attempt(1).unwrap().token().unwrap().clone();
    let second_token = carriers.attempt(2).unwrap().token().unwrap().clone();

    // The framed packets a dying attempt's lanes were still holding.
    let mut sender = PeerCarrier::opened(first.clone(), 0);
    sender
        .authenticate(&ready(&first, &["session-a"], 7), "conn-a".to_owned())
        .unwrap();
    sender.enqueue(PeerLane::Data, vec![7u8; 4_000]).unwrap();
    let mut framed = Vec::new();
    while let Some(packet) = sender.next_packet(PeerLane::Data).unwrap() {
        framed.push(packet);
    }
    assert!(!framed.is_empty());

    let retired = carriers.retire_attempt(1).expect("the attempt was held");
    assert_eq!(retired.connection_id(), Some("conn-a"));
    let settled = framed
        .iter()
        .filter(|packet| {
            carriers
                .attempt_mut(1)
                .is_some_and(|carrier| carrier.push(PeerLane::Data, 0, packet).is_ok())
        })
        .count();
    assert_eq!(
        settled, 0,
        "a packet naming a retired attempt finds no carrier to settle it against"
    );
    assert!(carriers.attempt(1).is_none());
    assert!(
        carriers.for_token(&first_token).is_none(),
        "its generation no longer reaches any carrier, so a replay of its frame \
         has nothing to fold against"
    );
    assert_eq!(
        carriers
            .for_token(&second_token)
            .map(PeerCarrier::attempt_id),
        Some(2),
        "the other worker's carrier is untouched by one worker's retire"
    );
}

/// A carrier displaced from its worker's slot cannot be PROMOTED again, even
/// while the candidate staged for it still names the very token it spent.
///
/// The registry holds no blacklist of displaced generations, and it must not
/// hold one: `RouteRegistry::register` deliberately lets a newcomer take a
/// candidate slot that serves nothing (`registry/mod.rs:74-80`), because that
/// is the rule which lets a loopback carrier win over a peer that has proved
/// nothing yet — `smoke/terminal/terminal-peer.spec.ts:62`. The fence is
/// therefore in `promote`, where the candidate's own connection id is compared
/// against the connection currently holding the worker's slot.
///
/// This is the fence the replay test depends on: if the displaced connection
/// could promote on the generation it already spent, a frame folded against a
/// dead attempt would satisfy every remaining precondition.
#[test]
fn a_promotion_token_cannot_be_reused_across_attempts() {
    let first = attempt(1, "worker-a", &["session-a"]);
    let newer = attempt(2, "worker-a", &["session-a"]);
    let mut carriers = PeerCarriers::new();
    let (first_carrier, _) = carriers
        .authenticate_after_open(
            &first,
            &ready(&first, &["session-a"], 7),
            "conn-a".to_owned(),
        )
        .expect("the first attempt authenticates");
    let (newer_carrier, displaced) = carriers
        .authenticate_after_open(
            &newer,
            &ready(&newer, &["session-a"], 8),
            "conn-b".to_owned(),
        )
        .expect("the second attempt authenticates");

    assert_ne!(
        first_carrier.token, newer_carrier.token,
        "two attempts are two generations, and one socket generation cannot name both"
    );
    assert_eq!(
        displaced
            .expect("the newer attempt displaced the older one")
            .attempt_id(),
        1,
        "the displaced record is the attempt that was staging, by its own id"
    );

    let mut registry = RouteRegistry::new();
    registry.set_view_demand("worker-a", "session-a", "view-a", true);
    assert!(registry.register(first_carrier.clone()).accepted);
    assert!(
        stage_candidate(&mut registry, 1, &first_carrier),
        "a complete candidate with nothing unacknowledged is stageable"
    );
    registry
        .promote("session-a", 1, &first_carrier.token)
        .expect(
            "the first attempt's complete candidate promotes; the refusal below is not vacuous",
        );

    // The newer attempt takes the worker's CANDIDATE slot while the first still
    // serves the elected route, which is exactly the displacement under test.
    assert!(
        registry.register(newer_carrier).accepted,
        "a newcomer may take a slot that serves nothing, which is the rule that \
         lets loopback displace a peer that has proved nothing"
    );

    // The displaced attempt stages again — `stage` refuses only an attempt OLDER
    // than the one already staged — and it carries the very token it spent.
    assert!(stage_candidate(&mut registry, 1, &first_carrier));
    assert_eq!(
        registry.staged_token("session-a"),
        Some(first_carrier.token.clone()),
        "the staged candidate really is folding on that generation, so the \
         refusal below is about the SLOT and not about an absent candidate"
    );
    assert_eq!(
        registry.promote("session-a", 1, &first_carrier.token).err(),
        Some(PromotionRefusal::TokenChanged),
        "the connection that generation names is in no slot any more, so it cannot \
         become this session's route a second time"
    );
}

/// Stage one attempt's candidate with a complete baseline and nothing left to
/// acknowledge, which is the only shape `promote` is willing to act on.
fn stage_candidate(registry: &mut RouteRegistry, attempt_id: u64, carrier: &DirectCarrier) -> bool {
    registry.stage(
        PromotionCandidate {
            session_id: "session-a".to_owned(),
            connection_id: carrier.connection_id.clone(),
            token: carrier.token.clone(),
            attempt_id,
            baseline_ready: true,
            prospective_views: BTreeMap::new(),
            staged_at_ms: 0,
        },
        TerminalSession::new("session-a", carrier.worker_fp.clone()),
    )
}
