#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Staging a direct candidate: what an authenticated carrier asks the host for,
//! and every way that answer can be refused.
//!
//! The order this file pins is the order the contract requires. `CarrierReady`
//! asks the HOST for a view id and publishes nothing — a view published under
//! the pane's own id is a second live socket on one handle, and the worker
//! refuses it. The answer publishes that id, and Sync still holds its own:
//! staging is not electing and is not parking.
//!
//! `direct_carrier_promotion.rs` drives what happens after this, and
//! `direct_carrier_route_loss.rs` what happens when the carrier goes away.

mod direct_carrier_support;

use std::collections::BTreeSet;

use direct_carrier_support::*;
use roost_client_core::client::carriers::DirectGrant;

#[test]
fn an_admitted_carrier_asks_the_host_for_a_view_id_and_publishes_nothing_yet() {
    let mut core = core_with_a_pane();

    let effects = core.handle(ClientEvent::CarrierReady(carrier(&[SESSION])));

    assert_eq!(
        mint_attempt(&effects),
        1,
        "the staged attempt asks the host for an id"
    );
    assert!(
        matches!(
            effects.iter().find(|effect| matches!(effect, Effect::MintTerminalViewId { .. })),
            Some(Effect::MintTerminalViewId {
                session_id,
                logical_view_id,
                target: ViewIdTarget::Candidate,
                ..
            }) if session_id == SESSION && logical_view_id == VIEW
        ),
        "the request names the session and the PANE, not an id; got {effects:?}"
    );
    assert_eq!(
        direct_publishes(&effects),
        Vec::new(),
        "nothing may be published before there is an id the worker has never seen; \
         got {effects:?}"
    );
}

#[test]
fn a_carrier_asks_for_ids_only_for_the_sessions_its_grant_admits() {
    let mut core = core_with_a_pane();
    let _ = core.handle(viewing(OTHER_SESSION, OTHER_VIEW, COLS, ROWS));

    let effects = core.handle(ClientEvent::CarrierReady(carrier(&[SESSION])));

    let asked: Vec<String> = effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::MintTerminalViewId {
                session_id,
                logical_view_id,
                ..
            } => Some(format!("{session_id}/{logical_view_id}")),
            _ => None,
        })
        .collect();
    assert_eq!(
        asked,
        vec![format!("{SESSION}/{VIEW}")],
        "a grant admits its own session set and nothing else; got {effects:?}"
    );
}

#[test]
fn publishing_a_candidate_does_not_elect_it() {
    let mut core = core_with_a_pane();

    let staged = core.handle(ClientEvent::CarrierReady(carrier(&[SESSION])));
    let _ = minted(&mut core, mint_attempt(&staged), Some(WIRE));

    assert!(
        core.store().routes.candidate(SESSION).is_some(),
        "the candidate is staged and holds the token the view went out on"
    );
    assert!(
        core.store().routes.route(SESSION).is_none(),
        "no baseline has arrived, so nothing may be elected yet"
    );
}

#[test]
fn the_minted_id_is_what_the_candidate_publishes_and_sync_keeps_its_own() {
    let mut core = core_with_a_pane();
    let staged = core.handle(ClientEvent::CarrierReady(carrier(&[SESSION])));
    let effects = minted(&mut core, mint_attempt(&staged), Some(WIRE));

    assert_eq!(
        direct_publishes(&effects),
        vec![(SESSION.to_owned(), WIRE.to_owned())],
        "the candidate publishes the id the host minted, not the pane's own; got {effects:?}"
    );
    assert!(
        !sync_view_intents(&effects)
            .iter()
            .any(|(session, _, intent)| session == SESSION && *intent == ViewIntent::Unpublish),
        "staging releases nothing on Sync; the old view is still the one the \
         coordinator holds. Got {effects:?}"
    );
    assert!(
        effects
            .iter()
            .all(|effect| !matches!(effect, Effect::SendSync(SyncCommand::TerminalView { .. }))),
        "the candidate's publication must not be re-addressed to Sync at all; got {effects:?}"
    );
}

#[test]
fn a_host_that_mints_nothing_abandons_the_attempt_and_keeps_the_pane() {
    let mut core = core_with_a_pane();

    let effects = core.handle(ClientEvent::CarrierReady(carrier(&[SESSION])));
    let refused = minted(&mut core, 1, None);

    assert!(
        direct_publishes(&effects).is_empty(),
        "nothing was published, so nothing is released either"
    );
    assert!(
        direct_publishes(&refused).is_empty(),
        "a mint that returned nothing has no id to unpublish; got {refused:?}"
    );
    assert!(
        core.store().routes.candidate(SESSION).is_none(),
        "the attempt is abandoned rather than left waiting for an answer that \
         will never come"
    );
    assert!(
        core.store().routes.route(SESSION).is_none(),
        "and nothing is elected, so the pane stays on the route that was working"
    );
}

#[test]
fn a_minted_id_the_worker_would_refuse_abandons_the_attempt() {
    let mut core = core_with_a_pane();
    let _ = core.handle(ClientEvent::CarrierReady(carrier(&[SESSION])));

    let effects = minted(&mut core, 1, Some("22222222-2222-1222-8222-222222222222"));

    assert_eq!(
        direct_publishes(&effects),
        Vec::new(),
        "a v1 id is never published; got {effects:?}"
    );
    assert!(
        core.store().routes.candidate(SESSION).is_none(),
        "the attempt is abandoned rather than left holding an unusable id"
    );
}

#[test]
fn a_mint_that_collides_with_a_live_view_abandons_the_attempt() {
    let mut core = core_with_a_pane();
    let _ = core.handle(viewing(OTHER_SESSION, OTHER_VIEW, COLS, ROWS));
    let _ = core.handle(ClientEvent::CarrierReady(carrier(&[
        SESSION,
        OTHER_SESSION,
    ])));

    let effects = core.handle(ClientEvent::TerminalViewIdMinted {
        session_id: SESSION.to_owned(),
        attempt_id: 1,
        logical_view_id: VIEW.to_owned(),
        target: ViewIdTarget::Candidate,
        wire_view_id: Some(OTHER_WIRE.to_owned()),
    });
    // Hand the second session's pane the same id, so the first candidate's id is
    // a collision with a view this document really does hold.
    let _ = core.handle(ClientEvent::TerminalViewIdMinted {
        session_id: OTHER_SESSION.to_owned(),
        attempt_id: 2,
        logical_view_id: OTHER_VIEW.to_owned(),
        target: ViewIdTarget::Candidate,
        wire_view_id: Some(OTHER_WIRE.to_owned()),
    });
    let _ = effects;

    let collision = core.handle(ClientEvent::TerminalViewIdMinted {
        session_id: SESSION.to_owned(),
        attempt_id: 3,
        logical_view_id: VIEW.to_owned(),
        target: ViewIdTarget::Candidate,
        wire_view_id: Some(OTHER_WIRE.to_owned()),
    });

    assert_eq!(
        direct_publishes(&collision),
        Vec::new(),
        "an id another live view already holds is never published; got {collision:?}"
    );
}

#[test]
fn a_stale_or_repeated_mint_is_ignored() {
    let mut core = core_with_a_pane();
    let _ = core.handle(ClientEvent::CarrierReady(carrier(&[SESSION])));
    let _ = minted(&mut core, 1, Some(WIRE));

    let stale = core.handle(ClientEvent::TerminalViewIdMinted {
        session_id: SESSION.to_owned(),
        attempt_id: 99,
        logical_view_id: VIEW.to_owned(),
        target: ViewIdTarget::Candidate,
        wire_view_id: Some(OTHER_WIRE.to_owned()),
    });
    let repeated = minted(&mut core, 1, Some(OTHER_WIRE));

    assert!(
        direct_publishes(&stale).is_empty(),
        "an attempt that has moved on publishes nothing; got {stale:?}"
    );
    assert!(
        direct_publishes(&repeated).is_empty(),
        "a pane that already has an id does not get a second; got {repeated:?}"
    );
    let published = direct_publishes(&core.handle(direct_frame(baseline())));
    assert_eq!(
        published,
        Vec::new(),
        "and the candidate is still folding the first id it published"
    );
}

/// A second session on a worker joins the carrier the first one opened.
///
/// The carrier authenticated on a grant naming one session; the refreshed grant
/// names both. v2 hands it to the live connection (`updateGrant`) and stages the
/// demanded sessions on it — without that, the second session waits on a
/// negotiation that never runs again, because the worker already has its peer.
#[test]
fn a_refreshed_grant_widens_the_live_carrier_and_stages_only_what_it_added() {
    let mut core = core_with_a_pane();
    let _ = core.handle(viewing(OTHER_SESSION, OTHER_VIEW, COLS, ROWS));
    let _ = core.handle(ClientEvent::CarrierReady(carrier(&[SESSION])));

    let effects = core.handle(ClientEvent::DirectGrantMinted {
        grant: DirectGrant {
            grant_id: "grant-a".to_owned(),
            secret: "secret-a".to_owned(),
            worker_fp: WORKER.to_owned(),
            worker_epoch: PROCESS_EPOCH.to_owned(),
            tab_id: "tab-a".to_owned(),
            device_fingerprint: "device-a".to_owned(),
            session_ids: BTreeSet::from([SESSION.to_owned(), OTHER_SESSION.to_owned()]),
            peer_supported: true,
            input_route_supported: true,
            stun_urls: Vec::new(),
            expires_at_ms: u64::MAX,
        },
    });

    let asked: Vec<String> = effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::MintTerminalViewId { session_id, .. } => Some(session_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        asked,
        vec![OTHER_SESSION.to_owned()],
        "the widened carrier stages the session the grant added, and only it; got {effects:?}"
    );
    assert!(
        core.store()
            .routes
            .granted_sessions_for(&direct_token())
            .is_some_and(|granted| granted.contains(OTHER_SESSION)),
        "the send path reads the widened scope, so the new session's commands are not refused"
    );
}
