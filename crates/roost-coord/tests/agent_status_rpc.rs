//! The observed-agent read RPCs against a real migrated coordinator: who may
//! ask, which session boundary a caller can and cannot see, and the exact
//! outcomes a client acts on. The default-agent config RPCs are in
//! `agent_config_rpc.rs`.
//!
//! The authorization order matters here and is the point of several of these
//! cases: the durable session boundary is consulted before the volatile status
//! is, so a caller cannot use an error's shape to find out which session ids
//! exist.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod agent_fixture;

use std::time::Duration;

use agent_fixture::{
    AgentFixture, EPOCH_A, OCCUPANT_A, OCCUPANT_B, SESSION_IDS, SESSION_MISSING, WORKER_A,
    legacy_status, session, status, worker,
};
use connectrpc::ErrorCode;
use roost_coord::agents::rpc_status::{
    handle_agent_config_get, handle_agent_config_set, handle_agent_status_get,
    handle_agent_status_list, handle_agent_status_wait,
};
use roost_proto as proto;
use serde_json::json;

fn retain(fixture: &AgentFixture, worker_fp: &str, session_id: &str, overrides: serde_json::Value) {
    let accepted = fixture.hub().accept_worker_status(
        &fixture.core,
        &worker(worker_fp),
        status(session_id, overrides.clone()),
    );
    assert_eq!(
        accepted,
        roost_coord::agents::status_hub::AgentStatusAcceptance::Accepted,
        "{session_id} {overrides} must be accepted"
    );
}

fn get_request(session_id: &str) -> proto::AgentStatusGetRequest {
    proto::AgentStatusGetRequest {
        session_id: session_id.to_owned(),
        ..Default::default()
    }
}

fn wait_request(
    session_id: &str,
    states: &[&str],
    timeout_ms: u32,
) -> proto::AgentStatusWaitRequest {
    proto::AgentStatusWaitRequest {
        session_id: session_id.to_owned(),
        status_epoch: EPOCH_A.to_owned(),
        occupant_id: OCCUPANT_A.to_owned(),
        desired_states: states.iter().map(|state| (*state).to_owned()).collect(),
        after_revision: None,
        timeout_ms,
        ..Default::default()
    }
}

#[tokio::test]
async fn every_method_refuses_a_caller_that_is_not_a_browser() {
    let fixture = AgentFixture::new("rpc-auth").await;
    let caller = &fixture.worker_caller;
    for refusal in [
        handle_agent_status_get(&fixture.core, caller, get_request(SESSION_IDS[0]))
            .await
            .map(|_| ()),
        handle_agent_status_list(&fixture.core, caller, proto::AgentStatusListRequest::default())
            .await
            .map(|_| ()),
        handle_agent_status_wait(
            &fixture.core,
            caller,
            wait_request(SESSION_IDS[0], &["blocked"], 1_000),
        )
        .await
        .map(|_| ()),
        handle_agent_config_get(&fixture.core, caller, proto::AgentConfigGetRequest::default())
            .await
            .map(|_| ()),
        handle_agent_config_set(
            &fixture.core,
            caller,
            proto::AgentConfigSetRequest::default(),
        )
        .await
        .map(|_| ()),
    ] {
        let error = refusal.expect_err("a worker identity is not a device");
        assert_eq!(error.code, ErrorCode::Unauthenticated);
    }
}

#[tokio::test]
async fn a_retained_status_stays_readable_after_its_worker_route_goes_offline() {
    let fixture = AgentFixture::new("rpc-offline").await;
    retain(
        &fixture,
        WORKER_A,
        SESSION_IDS[0],
        json!({"revision": 7, "state": "idle", "completed_revision": 4, "message": "review ready"}),
    );
    // The machine is gone. The observation is volatile but the session is not,
    // so the status the user was looking at must not vanish with the socket.
    fixture
        .core
        .services
        .byte_hub
        .evict_session(&session(SESSION_IDS[0]));

    let response =
        handle_agent_status_get(&fixture.core, &fixture.caller, get_request(SESSION_IDS[0]))
            .await
            .expect("a retained status")
            .body;
    let status = response.status.as_option().expect("a status view");
    assert_eq!(status.session_id, SESSION_IDS[0]);
    assert_eq!(status.state, "idle");
    assert_eq!(status.revision, 7);
    assert_eq!(status.completed_revision, 4);
    assert_eq!(status.message.as_deref(), Some("review ready"));
    assert_eq!(status.status_epoch.as_deref(), Some(EPOCH_A));
    assert_eq!(status.occupant_id.as_deref(), Some(OCCUPANT_A));
    assert_eq!(status.source.as_deref(), Some("integration"));
    assert!(status.promptable, "an integration occupant is promptable");
}

#[tokio::test]
async fn a_missing_session_and_a_statusless_session_are_one_answer() {
    let fixture = AgentFixture::new("rpc-notfound").await;
    let missing =
        handle_agent_status_get(&fixture.core, &fixture.caller, get_request(SESSION_MISSING))
            .await
            .expect_err("a session that never existed");
    let statusless =
        handle_agent_status_get(&fixture.core, &fixture.caller, get_request(SESSION_IDS[2]))
            .await
            .expect_err("an open session with no agent");
    assert_eq!(missing.code, ErrorCode::NotFound);
    assert_eq!(missing.code, statusless.code);
    assert_eq!(
        missing.message, statusless.message,
        "the two must be indistinguishable, or the error shape is an oracle"
    );
}

#[tokio::test]
async fn a_closed_session_stops_answering_even_while_its_status_is_retained() {
    let fixture = AgentFixture::new("rpc-closed").await;
    retain(&fixture, WORKER_A, SESSION_IDS[0], json!({"revision": 2}));
    fixture
        .exec(&format!(
            "UPDATE sessions SET status = 'closed', closed_at_ms = 2000 WHERE id = '{}'",
            SESSION_IDS[0]
        ))
        .await;
    let refused =
        handle_agent_status_get(&fixture.core, &fixture.caller, get_request(SESSION_IDS[0]))
            .await
            .expect_err("a closed session");
    assert_eq!(refused.code, ErrorCode::NotFound);

    let listed = handle_agent_status_list(
        &fixture.core,
        &fixture.caller,
        proto::AgentStatusListRequest::default(),
    )
    .await
    .expect("a list")
    .body;
    assert!(
        listed.statuses.is_empty(),
        "a retained status for a session that is no longer open is not published"
    );
}

#[tokio::test]
async fn the_list_answers_in_session_id_order_with_derived_promptability() {
    let fixture = AgentFixture::new("rpc-list").await;
    // Filled in an order that is not the answer's order.
    retain(&fixture, WORKER_A, SESSION_IDS[2], json!({"revision": 9}));
    // A worker deployed before durable observation reports no identity at all.
    fixture.hub().accept_worker_status(
        &fixture.core,
        &worker(WORKER_A),
        legacy_status(
            SESSION_IDS[0],
            json!({"revision": 5, "completed_revision": 1, "message": "legacy worker"}),
        ),
    );
    retain(
        &fixture,
        WORKER_A,
        SESSION_IDS[1],
        json!({"revision": 7, "state": "idle", "completed_revision": 4}),
    );
    retain(
        &fixture,
        WORKER_A,
        SESSION_IDS[3],
        json!({"revision": 2, "state": "blocked", "source": "screen"}),
    );
    // The identified frame for the same session is refused: once an identified
    // occupant exists, a legacy one can no longer answer for the session.
    let refused = fixture.hub().accept_worker_status(
        &fixture.core,
        &worker(WORKER_A),
        status(SESSION_IDS[0], json!({"revision": 6})),
    );
    assert_eq!(
        refused,
        roost_coord::agents::status_hub::AgentStatusAcceptance::Stale
    );
    retain(
        &fixture,
        WORKER_A,
        SESSION_IDS[1],
        json!({"revision": 8, "occupant_exited": true}),
    );

    let listed = handle_agent_status_list(
        &fixture.core,
        &fixture.caller,
        proto::AgentStatusListRequest::default(),
    )
    .await
    .expect("a list")
    .body;
    let rows: Vec<(&str, bool)> = listed
        .statuses
        .iter()
        .map(|view| (view.session_id.as_str(), view.promptable))
        .collect();
    assert_eq!(
        rows,
        [
            (SESSION_IDS[1], false),
            (SESSION_IDS[3], false),
            (SESSION_IDS[0], false),
            (SESSION_IDS[2], true),
        ],
        "session-id order, and only a live integration occupant is promptable"
    );
    let legacy = &listed.statuses[2];
    assert_eq!(
        legacy.status_epoch, None,
        "a legacy row carries no identity, and inventing one would fence nothing"
    );
    assert_eq!(legacy.occupant_id, None);
    assert_eq!(legacy.source, None);
}

#[tokio::test]
async fn a_wait_answers_with_the_outcome_the_client_asked_about() {
    let fixture = AgentFixture::new("rpc-wait").await;
    retain(
        &fixture,
        WORKER_A,
        SESSION_IDS[0],
        json!({"revision": 4, "state": "working"}),
    );
    let matched = handle_agent_status_wait(
        &fixture.core,
        &fixture.caller,
        wait_request(SESSION_IDS[0], &["working"], 30_000),
    )
    .await
    .expect("a settled wait")
    .body;
    assert_eq!(matched.outcome, "matched");

    // Nothing will move this agent, so the client's own budget is the answer.
    let timed_out = handle_agent_status_wait(
        &fixture.core,
        &fixture.caller,
        wait_request(SESSION_IDS[0], &["blocked"], 1),
    )
    .await
    .expect("a settled wait")
    .body;
    assert_eq!(timed_out.outcome, "timed_out");
    assert_eq!(
        fixture.hub().wait_count(),
        0,
        "a timed-out wait must not hold its slot"
    );
}

#[tokio::test]
async fn a_wait_never_becomes_an_oracle_for_which_sessions_exist() {
    let fixture = AgentFixture::new("rpc-wait-oracle").await;
    let missing = handle_agent_status_wait(
        &fixture.core,
        &fixture.caller,
        wait_request(SESSION_MISSING, &["blocked"], 0),
    )
    .await
    .expect_err("a session that never existed");
    assert_eq!(missing.code, ErrorCode::NotFound);
    assert_eq!(fixture.hub().wait_count(), 0);

    for malformed in [
        wait_request(SESSION_IDS[0], &[], 1_000),
        wait_request(SESSION_IDS[0], &["done"], 1_000),
        wait_request(SESSION_IDS[0], &["idle"], 0),
    ] {
        let refused = handle_agent_status_wait(&fixture.core, &fixture.caller, malformed)
            .await
            .expect_err("a malformed wait");
        assert_eq!(refused.code, ErrorCode::InvalidArgument, "{refused:?}");
    }
    let mut oversized = wait_request(SESSION_IDS[0], &["idle"], 1_000);
    oversized.after_revision = Some(9_007_199_254_740_992);
    let refused = handle_agent_status_wait(&fixture.core, &fixture.caller, oversized)
        .await
        .expect_err("a revision past the safe integer range");
    assert_eq!(refused.code, ErrorCode::InvalidArgument);
    assert_eq!(fixture.hub().wait_count(), 0);
}

#[tokio::test]
async fn a_wait_that_outlives_its_subscriber_leaves_nothing_behind() {
    let fixture = AgentFixture::new("rpc-wait-drop").await;
    retain(
        &fixture,
        WORKER_A,
        SESSION_IDS[0],
        json!({"revision": 1, "state": "working"}),
    );
    // The browser navigates away while the handler is parked on the wait. The
    // shape is the same as a cancelled Connect request: the future is dropped
    // mid-await, and the registry entry must go with it.
    {
        let mut parked = std::pin::pin!(handle_agent_status_wait(
            &fixture.core,
            &fixture.caller,
            wait_request(SESSION_IDS[0], &["blocked"], 300_000),
        ));
        let elapsed = tokio::time::timeout(Duration::from_millis(50), &mut parked).await;
        assert!(
            elapsed.is_err(),
            "the wait is still parked when the caller leaves"
        );
        assert_eq!(fixture.hub().wait_count(), 1, "and it is registered");
    }
    assert_eq!(
        fixture.hub().wait_count(),
        0,
        "an abandoned wait is released, not left to expire five minutes later"
    );
}

#[tokio::test]
async fn an_occupant_can_only_be_read_while_it_is_still_the_retained_one() {
    let fixture = AgentFixture::new("rpc-occupant").await;
    retain(
        &fixture,
        WORKER_A,
        SESSION_IDS[0],
        json!({"revision": 1, "state": "working"}),
    );
    let epoch = roost_protocol::wire::StatusEpoch::try_from(EPOCH_A).expect("an epoch");
    let occupant =
        roost_protocol::wire::AgentOccupantId::try_from(OCCUPANT_A).expect("an occupant");
    let replacement =
        roost_protocol::wire::AgentOccupantId::try_from(OCCUPANT_B).expect("an occupant");
    let held = session(SESSION_IDS[0]);
    assert_eq!(
        fixture
            .hub()
            .retained_occupant_state(&held, &epoch, &occupant),
        Some(roost_protocol::wire::AgentRuntimeState::Working)
    );
    assert_eq!(
        fixture
            .hub()
            .retained_occupant_state(&held, &epoch, &replacement),
        None,
        "a different occupant is not this one's activity"
    );
    retain(
        &fixture,
        WORKER_A,
        SESSION_IDS[0],
        json!({"revision": 1, "occupant_id": OCCUPANT_B}),
    );
    assert_eq!(
        fixture
            .hub()
            .retained_occupant_state(&held, &epoch, &occupant),
        None,
        "a replaced occupant's state stops being the session's"
    );
}
