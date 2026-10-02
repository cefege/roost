//! What a peer attempt does while the grant it rides on is being re-minted.
//!
//! The coordinator installs exactly the scope a mint names under the tab's one
//! grant id, so the worker always holds the LAST mint it served. Two mints in
//! flight at once, or an attempt that outlives the grant it opened on, leave the
//! page presenting one scope while the worker proves another, and every `Ready`
//! is refused. Ported from v2's `refreshAgain` coalescing and `updateGrant`
//! (`local-terminal-grants.ts`, `terminal-peer-connection.ts`).
//!
//! Depends on `tests/terminal_peer_support` for the described machine.

mod terminal_peer_support;
use std::collections::BTreeSet;

use roost_client_core::Effect;
use roost_client_core::client::carriers::{CarrierEffect, PeerAnswer, SignallingInput};
use terminal_peer_support::{
    EPOCH, FIRST_PEER_ID, demand, elsewhere, faults, grant_minted, machine, ready, usable_sdp,
};

const FIRST: &str = "session-first";
const SECOND: &str = "session-second";

fn mints(effects: &[CarrierEffect]) -> Vec<Vec<String>> {
    let requested = |effect: &CarrierEffect| match effect {
        CarrierEffect::Core(Effect::RequestDirectGrant { session_ids, .. }) => {
            Some(session_ids.clone())
        }
        _ => None,
    };
    effects.iter().filter_map(requested).collect()
}

fn opened_scope(effects: &[CarrierEffect]) -> Option<BTreeSet<String>> {
    effects.iter().find_map(|effect| match effect {
        CarrierEffect::OpenTransport { attempt } => Some(attempt.session_ids.clone()),
        _ => None,
    })
}

fn scope(sessions: &[&str]) -> BTreeSet<String> {
    sessions.iter().map(|id| id.to_string()).collect()
}

fn released(session: &str) -> SignallingInput {
    SignallingInput::Demand {
        session_id: session.to_string(),
        view_id: format!("view-{session}"),
        active: false,
    }
}

#[test]
fn a_demand_added_while_a_mint_is_out_waits_for_it_then_asks_for_the_whole_set() {
    let mut peer = machine(0);
    peer.step(elsewhere());
    assert_eq!(
        mints(&peer.step(demand(FIRST))),
        vec![vec![FIRST.to_string()]]
    );
    assert!(
        mints(&peer.step(demand(SECOND))).is_empty(),
        "a second mint racing the first lets the narrower answer land last"
    );

    let narrow = peer.step(grant_minted(&[FIRST]));
    assert_eq!(
        mints(&narrow),
        vec![vec![FIRST.to_string(), SECOND.to_string()]],
        "the answer did not cover the grown demand, so one more mint names all of it"
    );
    assert_eq!(
        opened_scope(&narrow),
        None,
        "no peer opens on a scope the next mint is about to replace"
    );

    let whole = peer.step(grant_minted(&[FIRST, SECOND]));
    assert!(
        mints(&whole).is_empty(),
        "a covering answer owes nothing more"
    );
    assert_eq!(opened_scope(&whole), Some(scope(&[FIRST, SECOND])));
}

#[test]
fn a_ready_naming_the_granted_scope_is_admitted_when_the_demand_is_narrower() {
    let mut peer = machine(0);
    peer.step(demand(FIRST));
    peer.step(demand(SECOND));
    peer.step(released(FIRST));
    peer.step(grant_minted(&[FIRST, SECOND]));
    let opened = peer.step(elsewhere());
    assert_eq!(
        opened_scope(&opened),
        Some(scope(&[FIRST, SECOND])),
        "the attempt presents the grant's scope, which is what the worker installed"
    );
    peer.step(SignallingInput::OfferReady {
        attempt_id: 1,
        peer_id: FIRST_PEER_ID.to_string(),
        offer_sdp: usable_sdp(),
    });
    peer.step(SignallingInput::AnswerReceived {
        attempt_id: 1,
        answer: PeerAnswer {
            peer_id: FIRST_PEER_ID.to_string(),
            worker_epoch: EPOCH.to_string(),
            answer_sdp: usable_sdp(),
        },
    });
    let authenticated = peer.step(SignallingInput::PeerAuthenticated {
        attempt_id: 1,
        ready: ready(EPOCH, &[FIRST, SECOND]),
    });
    assert!(faults(&authenticated).is_empty(), "got {authenticated:?}");
    assert!(
        authenticated
            .iter()
            .any(|effect| matches!(effect, CarrierEffect::StageCarrier { .. })),
        "the peer is staged; got {authenticated:?}"
    );
}

#[test]
fn a_grant_that_grows_under_an_unproven_attempt_closes_it_and_renegotiates_on_the_new_one() {
    let mut peer = machine(0);
    peer.step(demand(FIRST));
    peer.step(grant_minted(&[FIRST]));
    assert_eq!(opened_scope(&peer.step(elsewhere())), Some(scope(&[FIRST])));
    peer.step(demand(SECOND));

    let grown = peer.step(grant_minted(&[FIRST, SECOND]));
    assert!(
        grown
            .iter()
            .any(|effect| matches!(effect, CarrierEffect::CloseAttempt { attempt_id: 1, .. })),
        "the attempt presenting the old scope is closed before its Ready is refused; got {grown:?}"
    );
    let retry_at = grown
        .iter()
        .find_map(|effect| match effect {
            CarrierEffect::RetryAt { at_ms } => Some(*at_ms),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the closed attempt owes a retry; got {grown:?}"));

    let retried = peer.step(SignallingInput::RetryDue { now_ms: retry_at });
    assert_eq!(opened_scope(&retried), Some(scope(&[FIRST, SECOND])));
}
