//! The two refusal tests of the agent-status wait surface, split out of
//! `agent_status_wait.rs` because the admitted-wait half and the refused-wait
//! half together overflow the file cap while each is a whole subject.
//!
//! A SUBMODULE AND NOT A SECOND BINARY, deliberately: these tests share the
//! crate-root `#![allow(clippy::unwrap_used, clippy::expect_used)]` and every
//! import with the file above, and a second `tests/*.rs` would compile as its
//! own crate and have to redeclare both.

use super::*;

#[tokio::test]
async fn a_malformed_wait_is_refused_before_it_is_registered() {
    let refused = |states: &[&str], after_revision: Option<i64>, timeout_ms: u64| {
        AgentStatusWaitRequest::new(
            SESSION_IDS[0],
            EPOCH_A,
            OCCUPANT_A,
            &states
                .iter()
                .map(|state| (*state).to_owned())
                .collect::<Vec<String>>(),
            after_revision,
            timeout_ms,
        )
        .expect_err("a malformed wait is refused")
    };
    assert_eq!(
        refused(&[], None, 1_000).kind(),
        AgentStatusWaitErrorKind::Invalid
    );
    assert_eq!(
        refused(&["idle", "idle"], None, 1_000).kind(),
        AgentStatusWaitErrorKind::Invalid,
        "a duplicated state is a client bug, not a wider wait"
    );
    assert_eq!(
        refused(&["done"], None, 1_000).kind(),
        AgentStatusWaitErrorKind::Invalid
    );
    assert_eq!(
        refused(&["idle"], Some(-1), 1_000).kind(),
        AgentStatusWaitErrorKind::Invalid
    );
    assert_eq!(
        refused(&["idle"], None, 0).kind(),
        AgentStatusWaitErrorKind::Invalid
    );
    assert_eq!(
        refused(&["idle"], None, AGENT_STATUS_WAIT_MAX_TIMEOUT_MS + 1).kind(),
        AgentStatusWaitErrorKind::Invalid,
        "a wait past the ceiling is refused rather than silently shortened"
    );
    let bad_epoch = AgentStatusWaitRequest::new(
        SESSION_IDS[0],
        "not-a-uuid",
        OCCUPANT_A,
        &["idle".to_owned()],
        None,
        1_000,
    )
    .expect_err("a malformed epoch is refused");
    assert_eq!(bad_epoch.kind(), AgentStatusWaitErrorKind::Invalid);

    let fixture = AgentFixture::new("wait-invalid").await;
    retain(&fixture, json!({"revision": 1}));
    assert_eq!(
        fixture.hub().wait_count(),
        0,
        "a refused request never occupies a slot"
    );
    // The wait's own budget is what the handler waits on, not a fixed timer.
    let waiter = fixture
        .hub()
        .wait_for_agent_status(wait_request(&["blocked"], None))
        .expect("an admitted wait");
    assert_eq!(
        waiter.timeout(),
        Duration::from_millis(AGENT_STATUS_WAIT_MAX_TIMEOUT_MS)
    );
    fixture.hub().stop();
}

#[tokio::test]
async fn a_capacity_refusal_reaches_the_client_as_resource_exhausted() {
    let fixture = AgentFixture::new("wait-wire-capacity").await;
    retain(&fixture, json!({"revision": 1, "state": "working"}));
    // Held for the same reason as in the per-session bound test above: a
    // discarded waiter is a released slot, so these 32 must all still be
    // registered when the handler below is asked for a 33rd.
    let held: Vec<_> = (0..AGENT_STATUS_WAIT_MAX_PER_SESSION)
        .map(|_| {
            fixture
                .hub()
                .wait_for_agent_status(wait_request(&["blocked"], None))
                .expect("an admitted wait")
        })
        .collect();
    // The client is told to stop asking, not that its request was malformed:
    // an `InvalidArgument` here would be a client that retries forever, because
    // nothing about the request it sent is wrong.
    let refused = handle_agent_status_wait(
        &fixture.core,
        &fixture.caller,
        proto::AgentStatusWaitRequest {
            session_id: SESSION_IDS[0].to_owned(),
            status_epoch: EPOCH_A.to_owned(),
            occupant_id: OCCUPANT_A.to_owned(),
            desired_states: vec!["blocked".to_owned()],
            after_revision: None,
            timeout_ms: 1_000,
            ..Default::default()
        },
    )
    .await
    .expect_err("the per-session bound is a refusal");
    assert_eq!(refused.code, ErrorCode::ResourceExhausted);
    assert_eq!(
        refused.message.as_deref(),
        Some("agent status wait capacity exhausted"),
        "the client is told the limit it hit"
    );
    drop(held);
    fixture.hub().stop();
    assert_eq!(fixture.hub().wait_count(), 0);
}