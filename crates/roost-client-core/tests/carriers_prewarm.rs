//! Pre-warm: a granted peer held ready for every online worker with an open
//! session before any pane asks, within the document's peer budget.
//!
//! The machine half is driven through `CarrierLane`, the selection through
//! `ClientCore` with a seeded store — the visibility edge is the one public
//! event that re-runs the selection without a Sync link in the picture.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod sidebar_support;
mod terminal_peer_support;

use std::collections::{BTreeMap, BTreeSet};

use roost_client_core::client::carriers::{
    CarrierEffect, CarrierLane, DirectGrant, GRANT_RENEW_MS, PeerAnswer, PeerPhase, ReadyTuple,
    SignallingInput,
};
use roost_client_core::store::{SessionMap, SessionStatus};
use roost_client_core::{ClientCore, ClientEvent, Effect};
use roost_protocol::terminal_peer::peer::{
    TERMINAL_PEER_MAX_CONNECTIONS_PER_BROWSER_DOCUMENT, TERMINAL_PEER_MAX_SESSIONS_PER_GRANT,
};
use sidebar_support::{session, worker};
use terminal_peer_support::{FIRST_PEER_ID as PEER_ID, usable_sdp as sdp};

const WORKER: &str = "worker-a";

fn ids(sessions: &[&str]) -> BTreeSet<String> {
    sessions.iter().map(|id| (*id).to_owned()).collect()
}

fn grant(sessions: &BTreeSet<String>) -> DirectGrant {
    DirectGrant {
        grant_id: "grant-a".to_owned(),
        secret: "secret-a".to_owned(),
        worker_fp: WORKER.to_owned(),
        worker_epoch: "epoch-a".to_owned(),
        tab_id: "tab-a".to_owned(),
        device_fingerprint: "device-a".to_owned(),
        session_ids: sessions.clone(),
        peer_supported: true,
        input_route_supported: true,
        stun_urls: Vec::new(),
        expires_at_ms: u64::MAX,
    }
}

fn requested(effects: &[Effect]) -> Vec<(String, Vec<String>)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::RequestDirectGrant {
                session_ids,
                worker_fp,
            } => Some((worker_fp.clone(), session_ids.clone())),
            _ => None,
        })
        .collect()
}

fn opened_attempt(effects: &[Effect]) -> Option<u64> {
    effects.iter().find_map(|effect| match effect {
        Effect::Carrier(inner) => match &**inner {
            CarrierEffect::OpenTransport { attempt } => Some(attempt.attempt_id),
            _ => None,
        },
        _ => None,
    })
}

fn closed_attempt(effects: &[Effect]) -> bool {
    effects.iter().any(|effect| {
        matches!(effect, Effect::Carrier(inner)
            if matches!(**inner, CarrierEffect::CloseAttempt { .. }))
    })
}

/// A lane whose pre-warmed peer for `sessions` has authenticated, with no view
/// on the worker at all.
fn warm_lane(sessions: &BTreeSet<String>) -> CarrierLane {
    let mut lane = CarrierLane::new();
    let mut out = Vec::new();
    lane.set_environment(true, 0);
    lane.set_prewarm(WORKER, sessions.clone(), 0, &mut out);
    lane.local_door_answered(WORKER, "", &mut out);
    lane.grant_minted(grant(sessions), &mut out);
    let attempt_id = opened_attempt(&out).expect("a live grant opens the pre-warmed peer");
    let inputs = [
        SignallingInput::OfferReady {
            attempt_id,
            peer_id: PEER_ID.to_owned(),
            offer_sdp: sdp(),
        },
        SignallingInput::AnswerReceived {
            attempt_id,
            answer: PeerAnswer {
                peer_id: PEER_ID.to_owned(),
                worker_epoch: "epoch-a".to_owned(),
                answer_sdp: sdp(),
            },
        },
        SignallingInput::PeerAuthenticated {
            attempt_id,
            ready: ReadyTuple {
                worker_fp: WORKER.to_owned(),
                worker_epoch: "epoch-a".to_owned(),
                peer_id: PEER_ID.to_owned(),
                socket_generation: 7,
                socket_id: "socket-a".to_owned(),
                session_ids: sessions.clone(),
            },
        },
    ];
    for (input, now_ms) in inputs.into_iter().zip([30, 110, 130]) {
        lane.transport_observed(WORKER, input, now_ms, &mut Vec::new());
    }
    assert_eq!(lane.phase(WORKER), PeerPhase::Candidate);
    lane
}

#[test]
fn a_prewarm_with_no_view_mints_a_grant_for_its_sessions_and_opens_a_peer() {
    let sessions = ids(&["session-a", "session-b"]);
    let mut lane = CarrierLane::new();
    let mut out = Vec::new();
    lane.set_environment(true, 0);
    lane.set_prewarm(WORKER, sessions.clone(), 0, &mut out);
    assert_eq!(
        requested(&out),
        vec![(
            WORKER.to_owned(),
            vec!["session-a".to_owned(), "session-b".to_owned()]
        )],
        "one mint names every pre-warmed session"
    );

    let mut out = Vec::new();
    lane.local_door_answered(WORKER, "", &mut out);
    lane.grant_minted(grant(&sessions), &mut out);
    assert!(
        opened_attempt(&out).is_some(),
        "a live grant opens the peer with no view on the worker; got {out:?}"
    );
}

#[test]
fn an_observation_reported_through_the_lane_stamps_its_own_instant() {
    let lane = warm_lane(&ids(&["session-a"]));
    let phases = lane.snapshot(WORKER).direct_phase_ms;
    assert_eq!(
        (
            phases.negotiating_ms,
            phases.authenticating_ms,
            phases.candidate_ms
        ),
        (Some(30), Some(110), Some(130)),
        "the attempt opened at 0, so each stamp is the instant the lane was told"
    );
}

#[test]
fn a_view_session_is_never_the_one_left_out_of_a_full_grant() {
    let prewarmed: BTreeSet<String> = (0..TERMINAL_PEER_MAX_SESSIONS_PER_GRANT)
        .map(|index| format!("prewarm-{index:04}"))
        .collect();
    let mut lane = CarrierLane::new();
    lane.demand("viewed", WORKER, "view-1", true, 0, &mut Vec::new());
    lane.set_prewarm(WORKER, prewarmed, 0, &mut Vec::new());
    // The view's own mint is still out; its answer is when the grown demand,
    // coalesced behind it, is asked for.
    let mut out = Vec::new();
    lane.grant_minted(grant(&ids(&["viewed"])), &mut out);
    let (_, named) = requested(&out).pop().expect("the grown demand is minted");
    assert_eq!(named.len(), TERMINAL_PEER_MAX_SESSIONS_PER_GRANT);
    assert!(named.contains(&"viewed".to_owned()));
}

#[test]
fn a_cleared_prewarm_keeps_its_peer_and_stops_renewing_the_grant() {
    let sessions = ids(&["session-a"]);
    let mut lane = warm_lane(&sessions);
    let mut renewed = Vec::new();
    lane.sweep(GRANT_RENEW_MS, &mut renewed);
    assert_eq!(
        requested(&renewed).len(),
        1,
        "a pre-warmed grant is renewed like a viewed one"
    );
    lane.grant_minted(grant(&sessions), &mut Vec::new());

    let mut out = Vec::new();
    lane.clear_prewarm(WORKER, GRANT_RENEW_MS, &mut out);
    assert!(
        !closed_attempt(&out),
        "clearing keeps the peer; got {out:?}"
    );
    assert!(lane.snapshot(WORKER).has_carrier);
    let mut swept = Vec::new();
    lane.sweep(2 * GRANT_RENEW_MS + 1, &mut swept);
    assert!(
        requested(&swept).is_empty(),
        "nothing wants the grant any more, so it is not renewed; got {swept:?}"
    );
}

#[test]
fn a_released_prewarm_closes_its_peer_unless_a_view_holds_it() {
    let sessions = ids(&["session-a"]);
    let mut idle = warm_lane(&sessions);
    let mut out = Vec::new();
    idle.release_prewarm(WORKER, 1, &mut out);
    assert!(
        closed_attempt(&out),
        "an idle peer gives its slot back; got {out:?}"
    );
    assert!(!idle.snapshot(WORKER).has_carrier);

    let mut viewed = warm_lane(&sessions);
    viewed.demand("session-a", WORKER, "view-1", true, 1, &mut Vec::new());
    let mut out = Vec::new();
    viewed.release_prewarm(WORKER, 2, &mut out);
    assert!(
        !closed_attempt(&out),
        "a peer a view holds stays; got {out:?}"
    );
    assert!(viewed.snapshot(WORKER).has_carrier);
}

/// A worker fingerprint and its open sessions.
struct SeededWorker {
    fp: String,
    sessions: Vec<String>,
}

fn seeded_worker(index: usize, count: usize) -> SeededWorker {
    SeededWorker {
        fp: format!("{index:064x}"),
        sessions: (0..count)
            .map(|session| format!("00000000-0000-4000-8000-{index:06x}{session:06x}"))
            .collect(),
    }
}

/// A client whose store holds `workers`, all routable, with their sessions;
/// the ones named in `closed` are closed.
fn fleet(workers: &[SeededWorker], closed: &[&str]) -> ClientCore {
    let mut core = ClientCore::in_memory("tab-a");
    core.set_carrier_environment(true, 0);
    let store = core.store_mut();
    let mut map = SessionMap::new();
    for seeded in workers {
        store
            .workers
            .insert(seeded.fp.clone(), worker(&seeded.fp, &seeded.fp));
        for id in &seeded.sessions {
            let mut row = session(id, &seeded.fp, "/tmp", 1);
            if closed.contains(&id.as_str()) {
                row.status = SessionStatus::Closed;
            }
            map.insert(row.id.clone(), row);
        }
    }
    store.sessions.apply_snapshot(map);
    store.routable_worker_fps = Some(workers.iter().map(|seeded| seeded.fp.clone()).collect());
    core
}

fn prewarmed(core: &ClientCore) -> BTreeMap<String, usize> {
    let lane = &core.store().direct;
    lane.prewarmed_workers()
        .map(|fp| {
            let sessions = lane.prewarm_sessions(fp).map_or(0, BTreeSet::len);
            (fp.to_owned(), sessions)
        })
        .collect()
}

fn shown(core: &mut ClientCore, visible: bool) -> Vec<Effect> {
    core.handle(ClientEvent::PageVisibilityChanged { visible })
}

#[test]
fn the_selection_takes_the_workers_with_the_most_open_sessions_up_to_the_peer_cap() {
    let budget = TERMINAL_PEER_MAX_CONNECTIONS_PER_BROWSER_DOCUMENT;
    // One more worker than the cap. Workers 0 and 1 tie on one session each
    // at the cut, so the lower fingerprint keeps the slot and worker 1 is the
    // one left out; every other worker has more.
    let mut workers: Vec<SeededWorker> = (0..=budget)
        .map(|index| seeded_worker(index, index + 1))
        .collect();
    workers[1] = seeded_worker(1, 1);
    let mut core = fleet(&workers, &[]);
    let effects = shown(&mut core, true);

    let warm = prewarmed(&core);
    assert_eq!(warm.len(), budget, "exactly the cap; got {warm:?}");
    assert!(
        warm.contains_key(&workers[0].fp),
        "the tie goes to the lower fingerprint"
    );
    assert!(!warm.contains_key(&workers[1].fp));
    assert_eq!(warm[&workers[budget].fp], budget + 1);
    assert_eq!(
        requested(&effects).len(),
        budget,
        "one mint per warm worker"
    );

    let _ = shown(&mut core, false);
    assert!(
        prewarmed(&core).is_empty(),
        "a hidden document pre-warms nothing"
    );
}

#[test]
fn a_worker_with_no_open_session_is_never_prewarmed() {
    let busy = seeded_worker(1, 2);
    let idle = seeded_worker(2, 1);
    let closed = idle.sessions[0].clone();
    let mut core = fleet(&[busy, idle], &[closed.as_str()]);
    let effects = shown(&mut core, true);
    let asked: Vec<String> = requested(&effects)
        .into_iter()
        .map(|(worker_fp, _)| worker_fp)
        .collect();
    assert_eq!(asked, vec![format!("{:064x}", 1)]);
}

#[test]
fn a_view_on_an_unwarmed_worker_takes_the_slot_of_the_lowest_ranked_one() {
    let budget = TERMINAL_PEER_MAX_CONNECTIONS_PER_BROWSER_DOCUMENT;
    let workers: Vec<SeededWorker> = (0..=budget)
        .map(|index| seeded_worker(index, index + 1))
        .collect();
    let mut core = fleet(&workers, &[]);
    let _ = shown(&mut core, true);
    assert!(!prewarmed(&core).contains_key(&workers[0].fp));

    let _ = core.handle(ClientEvent::ViewOpened {
        session_id: workers[0].sessions[0].clone(),
        worker_fp: workers[0].fp.clone(),
        view_id: "view-1".to_owned(),
        cols: 80,
        rows: 24,
    });
    let warm = prewarmed(&core);
    assert_eq!(
        warm.len(),
        budget - 1,
        "the viewed worker holds one slot; got {warm:?}"
    );
    assert!(
        !warm.contains_key(&workers[1].fp),
        "and the lowest-ranked warm worker gives it up"
    );
}
