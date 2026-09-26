//! Per-device push dispatch for an agent transition: who gets told, who is
//! suppressed, and the identity-derived deduplication token.
//!
//! The viewer seam is a trait parameter here rather than a call into the
//! terminal domain, which does not exist yet. `NoTerminalViewers` is the value
//! a caller passes until it does, and it means "nobody is viewing".

#![allow(clippy::unwrap_used, clippy::expect_used)]
//!
//! The fences a push dispatch runs under, and the viewer seam it takes while
//! the terminal domain does not exist yet.
//!
//! A fence checked only before the batch lets a transition superseded while
//! the query was running notify the fleet anyway; checked again inside the
//! sender, it stops. `NoTerminalViewers` must mean "nobody is viewing" and
//! not "everyone is" -- the other reading silently suppresses every push in
//! the fleet until the terminal slice lands.

mod push_fixture;

use std::sync::Arc;

use push_fixture::{PUSH_ORIGIN, PushFixture, seed_open_session};
use roost_coord::push::dispatch::{
    ActiveTerminalViewers, AgentPushTransition, NoTerminalViewers, PushTransition,
    fire_push_for_transition,
};
use roost_protocol::wire::{AgentOccupantId, SessionId, StatusEpoch};

const STATUS_EPOCH: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const OCCUPANT_ID: &str = "11111111-aaaa-4aaa-8aaa-111111111111";

/// The fence a transition that is still the current one runs under.
fn always_current() -> Arc<dyn Fn() -> bool + Send + Sync> {
    Arc::new(|| true)
}

/// The fence a transition that has been superseded runs under.
fn never_current() -> Arc<dyn Fn() -> bool + Send + Sync> {
    Arc::new(|| false)
}

/// A transition for [`push_fixture::SESSION_ID`] at the given revision.
fn transition(kind: PushTransition, revision: u64) -> AgentPushTransition {
    AgentPushTransition {
        session_id: SessionId::try_from(push_fixture::SESSION_ID).expect("a session id"),
        kind,
        status_epoch: StatusEpoch::try_from(STATUS_EPOCH).expect("a status epoch"),
        occupant_id: AgentOccupantId::try_from(OCCUPANT_ID).expect("an occupant id"),
        revision,
    }
}

#[tokio::test]
async fn a_transition_that_is_already_superseded_sends_nothing() {
    let fixture = PushFixture::new("dispatch-superseded").await;
    seed_open_session(&fixture, "/work/project").await;
    fixture
        .seed_subscription(
            &fixture.dashboard_id,
            &fixture.fp(),
            &format!("{PUSH_ORIGIN}/stale"),
        )
        .await;
    let transport = push_fixture::FakeTransport::accepting();
    let superseded = never_current();

    fire_push_for_transition(
        fixture.database().pool(),
        &transition(PushTransition::Blocked, 1),
        &[PUSH_ORIGIN.to_owned()],
        &NoTerminalViewers,
        &superseded,
        transport.as_ref(),
    )
    .await;

    assert!(transport.deliveries().is_empty());
}

#[tokio::test]
async fn an_empty_allowlist_sends_nothing_at_all() {
    let fixture = PushFixture::new("dispatch-no-origins").await;
    seed_open_session(&fixture, "/work/project").await;
    fixture
        .seed_subscription(
            &fixture.dashboard_id,
            &fixture.fp(),
            &format!("{PUSH_ORIGIN}/unconfigured"),
        )
        .await;
    let transport = push_fixture::FakeTransport::accepting();
    let current = always_current();

    fire_push_for_transition(
        fixture.database().pool(),
        &transition(PushTransition::Blocked, 1),
        &[],
        &NoTerminalViewers,
        &current,
        transport.as_ref(),
    )
    .await;

    assert!(transport.deliveries().is_empty());
}

#[tokio::test]
async fn the_no_viewers_value_reports_nobody_as_watching() {
    // Until the terminal domain lands, every dispatch takes this value. It has
    // to mean "nobody is viewing" and not "everyone is": the alternative would
    // silently suppress every push in the fleet.
    let views = NoTerminalViewers.active_viewer_fingerprints(push_fixture::SESSION_ID);
    assert!(views.is_empty(), "no hub means nobody is watching");
}
