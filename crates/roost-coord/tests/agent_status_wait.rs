//! Agent status waits: what counts as progress, what ends a wait, the registry
//! a long-poll must not leak, and how a refusal reaches the client.
//!
//! A wait here is a question a browser asked and is still waiting on -- "is my
//! agent blocked yet". It is answered by the next change to the retained status
//! and by nothing else, and a browser that gives up must leave nothing behind.
//! The last case is here rather than in the handler file because the thing it
//! guards is the MAPPING: a per-session capacity refusal must reach the client
//! as `ResourceExhausted`, which is the only signal it has to stop asking.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod agent_fixture;

/// The two refusal tests are one compilation unit's worth of behaviour and not
/// one file's worth of lines: a submodule of this binary, so they keep the
/// crate-root allow and every import the admitted-wait half already declares.
#[path = "agent_status_wait/refusals.rs"]
mod refusals;

use std::time::Duration;

use agent_fixture::{
    AgentFixture, EPOCH_A, OCCUPANT_A, OCCUPANT_B, SESSION_IDS, WORKER_A, session, status, worker,
};
use connectrpc::ErrorCode;
use roost_coord::agents::rpc_status::handle_agent_status_wait;
use roost_coord::agents::status_wait::{
    AGENT_STATUS_WAIT_MAX_GLOBAL, AGENT_STATUS_WAIT_MAX_PER_SESSION,
    AGENT_STATUS_WAIT_MAX_TIMEOUT_MS, AgentStatusWaitCapacity, AgentStatusWaitErrorKind,
    AgentStatusWaitOutcome, AgentStatusWaitRegistry, AgentStatusWaitRequest, AgentStatusWaiter,
};

use roost_proto as proto;
use serde_json::json;

fn wait_request(states: &[&str], after_revision: Option<i64>) -> AgentStatusWaitRequest {
    AgentStatusWaitRequest::new(
        SESSION_IDS[0],
        EPOCH_A,
        OCCUPANT_A,
        &states
            .iter()
            .map(|state| (*state).to_owned())
            .collect::<Vec<String>>(),
        after_revision,
        AGENT_STATUS_WAIT_MAX_TIMEOUT_MS,
    )
    .expect("a valid wait")
}

/// Feed one accepted frame; a refused one would make the wait's silence look
/// like progress when it is really a fence.
fn retain(fixture: &AgentFixture, overrides: serde_json::Value) {
    let accepted = fixture.hub().accept_worker_status(
        &fixture.core,
        &worker(WORKER_A),
        status(SESSION_IDS[0], overrides.clone()),
    );
    assert_eq!(
        accepted,
        roost_coord::agents::status_hub::AgentStatusAcceptance::Accepted,
        "{overrides} must be accepted"
    );
}

#[tokio::test]
async fn a_wait_matches_the_retained_exact_occupant_without_waiting() {
    let fixture = AgentFixture::new("wait-match").await;
    retain(&fixture, json!({"revision": 1}));
    retain(&fixture, json!({"revision": 2, "state": "blocked"}));
    let waiter = fixture
        .hub()
        .wait_for_agent_status(wait_request(&["blocked"], None))
        .expect("an admitted wait");
    assert_eq!(
        waiter.settle().await.expect("a settled wait"),
        AgentStatusWaitOutcome::Matched {
            matched_revision: 2
        }
    );
    assert_eq!(fixture.hub().wait_count(), 0, "a match releases the wait");
}

#[tokio::test]
async fn an_active_state_wait_advances_only_on_a_real_transition() {
    let fixture = AgentFixture::new("wait-active").await;
    retain(&fixture, json!({"revision": 1, "state": "working"}));
    let waiter = fixture
        .hub()
        .wait_for_agent_status(wait_request(&["working"], Some(1)))
        .expect("an admitted wait");
    assert_eq!(fixture.hub().wait_count(), 1);

    // A message change and an authority-source change both republish the same
    // state. Neither is the turn the client is waiting for.
    retain(&fixture, json!({"revision": 2, "message": "still working"}));
    retain(&fixture, json!({"revision": 3, "source": "screen"}));
    assert_eq!(fixture.hub().wait_count(), 1, "a republish is not progress");
    retain(
        &fixture,
        json!({"revision": 4, "state": "idle", "completed_revision": 4}),
    );
    assert_eq!(
        fixture.hub().wait_count(),
        1,
        "another state is not a match"
    );
    retain(&fixture, json!({"revision": 5, "state": "working"}));
    assert_eq!(fixture.hub().wait_count(), 0, "the transition releases it");

    assert_eq!(
        waiter.settle().await.expect("a settled wait"),
        AgentStatusWaitOutcome::Matched {
            matched_revision: 5
        }
    );
}

#[tokio::test]
async fn a_settled_wait_advances_only_on_a_completed_turn() {
    let fixture = AgentFixture::new("wait-settled").await;
    retain(&fixture, json!({"revision": 1, "state": "working"}));
    retain(
        &fixture,
        json!({"revision": 2, "state": "idle", "completed_revision": 2}),
    );
    let waiter = fixture
        .hub()
        .wait_for_agent_status(wait_request(&["idle"], Some(2)))
        .expect("an admitted wait");
    // A higher revision at the same completed turn is a republish, not a
    // finished turn: the agent had already stopped before the client asked.
    retain(
        &fixture,
        json!({"revision": 3, "state": "idle", "source": "screen", "completed_revision": 2}),
    );
    assert_eq!(
        fixture.hub().wait_count(),
        1,
        "the same turn is not progress"
    );
    retain(
        &fixture,
        json!({"revision": 4, "state": "working", "completed_revision": 2}),
    );
    assert_eq!(fixture.hub().wait_count(), 1);
    retain(
        &fixture,
        json!({"revision": 5, "state": "idle", "completed_revision": 5}),
    );
    assert_eq!(
        waiter.settle().await.expect("a settled wait"),
        AgentStatusWaitOutcome::Matched {
            matched_revision: 5
        }
    );
}

#[tokio::test]
async fn a_released_wait_is_not_leaked_when_the_subscriber_is_gone() {
    let fixture = AgentFixture::new("wait-drop").await;
    retain(&fixture, json!({"revision": 1}));
    let waiters: Vec<_> = (0..AGENT_STATUS_WAIT_MAX_PER_SESSION)
        .map(|_| {
            fixture
                .hub()
                .wait_for_agent_status(wait_request(&["blocked"], None))
                .expect("an admitted wait")
        })
        .collect();
    assert_eq!(
        fixture.hub().wait_count(),
        AGENT_STATUS_WAIT_MAX_PER_SESSION
    );
    // The browser navigates away: every waiter is dropped, and nothing else
    // happens -- no timer, no frame, no shutdown.
    drop(waiters);
    assert_eq!(
        fixture.hub().wait_count(),
        0,
        "a dropped subscriber must not hold its slot"
    );
    // The slots are genuinely free again, which a leaked waiter would prevent.
    for _ in 0..AGENT_STATUS_WAIT_MAX_PER_SESSION {
        fixture
            .hub()
            .wait_for_agent_status(wait_request(&["blocked"], None))
            .expect("a slot freed by a dropped subscriber is reusable");
    }
    fixture.hub().stop();
    assert_eq!(fixture.hub().wait_count(), 0);
}

#[tokio::test]
async fn a_session_close_ends_a_wait_as_closed_rather_than_as_a_replacement() {
    let fixture = AgentFixture::new("wait-close").await;
    retain(&fixture, json!({"revision": 1}));
    let waiter = fixture
        .hub()
        .wait_for_agent_status(wait_request(&["blocked"], None))
        .expect("an admitted wait");
    fixture
        .hub()
        .note_session_closed(&fixture.core.services.buses, &session(SESSION_IDS[0]));
    assert_eq!(
        waiter.settle().await.expect("a settled wait"),
        AgentStatusWaitOutcome::SessionClosed,
        "the close wins over the synthetic deletion it publishes"
    );
    assert_eq!(fixture.hub().wait_count(), 0);
}

#[tokio::test]
async fn a_replaced_occupant_ends_a_wait_as_changed() {
    let fixture = AgentFixture::new("wait-replaced").await;
    retain(&fixture, json!({"revision": 1}));
    let waiter = fixture
        .hub()
        .wait_for_agent_status(wait_request(&["blocked"], None))
        .expect("an admitted wait");
    retain(
        &fixture,
        json!({"revision": 1, "occupant_id": OCCUPANT_B, "state": "blocked"}),
    );
    assert_eq!(
        waiter.settle().await.expect("a settled wait"),
        AgentStatusWaitOutcome::OccupantChanged,
        "a different agent is not the one this wait pinned"
    );
    assert_eq!(fixture.hub().wait_count(), 0);
}

#[tokio::test]
async fn a_session_runs_out_of_wait_slots_before_the_coordinator_does() {
    let fixture = AgentFixture::new("wait-capacity").await;
    retain(&fixture, json!({"revision": 1}));
    // The waiters are HELD, not created and forgotten. A waiter releases its
    // slot when it is dropped, so a loop that discarded each one registered
    // them one at a time and the bound below was never reached.
    let held: Vec<_> = (0..AGENT_STATUS_WAIT_MAX_PER_SESSION)
        .map(|_| {
            fixture
                .hub()
                .wait_for_agent_status(wait_request(&["blocked"], None))
                .expect("an admitted wait")
        })
        .collect();
    let refused = fixture
        .hub()
        .wait_for_agent_status(wait_request(&["blocked"], None))
        .expect_err("the per-session bound is a refusal");
    assert_eq!(refused.kind(), AgentStatusWaitErrorKind::Capacity);
    assert_eq!(refused.capacity(), Some(AgentStatusWaitCapacity::Session));
    drop(held);
    fixture.hub().stop();
}

#[tokio::test]
async fn the_coordinator_bounds_total_waits_across_sessions() {
    let registry = AgentStatusWaitRegistry::new();
    let request = |index: usize| {
        let session_id = format!("30000000-0000-4000-8000-{index:012x}");
        let occupant_id = format!("40000000-0000-4000-8000-{index:012x}");
        AgentStatusWaitRequest::new(
            &session_id,
            EPOCH_A,
            &occupant_id,
            &["idle".to_owned()],
            None,
            1_000,
        )
        .expect("a valid wait")
    };
    // 32 sessions' worth of waiters is exactly the global bound. The admitted
    // waiters are COLLECTED: `register(..).is_ok()` consumed each one, and a
    // consumed waiter deregisters on drop, so the registry was empty between
    // every registration and the bound never bound anything.
    let mut held: Vec<AgentStatusWaiter> = Vec::new();
    for index in 0..=(AGENT_STATUS_WAIT_MAX_GLOBAL / AGENT_STATUS_WAIT_MAX_PER_SESSION) {
        // A LOOP, NOT `all()`. Every registration must be attempted even after
        // one is refused, because the property under test is that the registry
        // holds exactly the global bound and refuses the next one — a
        // short-circuiting combinator would stop at the first refusal and the
        // bound would never be reached, so the test would pass without
        // exercising it.
        let mut outcome = true;
        for _ in 0..AGENT_STATUS_WAIT_MAX_PER_SESSION {
            match registry.register(request(index)) {
                Ok(waiter) => held.push(waiter),
                Err(_) => outcome = false,
            }
        }
        if index == AGENT_STATUS_WAIT_MAX_GLOBAL / AGENT_STATUS_WAIT_MAX_PER_SESSION {
            assert!(!outcome, "the global bound refuses the last session");
        } else {
            assert!(outcome, "session {index} fits inside the global bound");
        }
    }
    assert_eq!(registry.waiter_count(), AGENT_STATUS_WAIT_MAX_GLOBAL);
    registry.cancel_all();
    assert_eq!(registry.waiter_count(), 0, "stopping releases every wait");
    drop(held);
}
