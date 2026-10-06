//! The four wait-method tests of the agent-status RPC surface, split out of
//! `agent_status_rpc.rs` because the list half and the wait half together
//! overflow the file cap while each half is a whole subject on its own.
//!
//! A SUBMODULE AND NOT A SECOND BINARY, deliberately: these tests share the
//! crate-root `#![allow(clippy::unwrap_used, clippy::expect_used)]` and every
//! import with the file above, and a second `tests/*.rs` would compile as its
//! own crate and have to redeclare both.

use super::*;

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
        let elapsed = tokio::time::timeout(Duration::from_secs(1), &mut parked).await;
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
