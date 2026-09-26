//! Per-device push dispatch for an agent transition: who gets told, who is
//! suppressed, and the identity-derived deduplication token.
//!
//! The viewer seam is a trait parameter here rather than a call into the
//! terminal domain, which does not exist yet. `NoTerminalViewers` is the value
//! a caller passes until it does, and it means "nobody is viewing".

#![allow(clippy::unwrap_used, clippy::expect_used)]
//!
//! What a Web Push notification says: the title a human reads, the body that
//! distinguishes a blocked agent from a finished one, and the identity-
//! derived deduplication token that lets a second revision replace a stale
//! notification rather than stack behind it.

mod push_fixture;

use std::sync::Arc;

use push_fixture::{PUSH_ORIGIN, PushFixture, seed_open_session};
use roost_coord::push::dispatch::{
    AgentPushTransition, NoTerminalViewers, PushTransition, fire_push_for_transition,
};
use roost_protocol::wire::{AgentOccupantId, SessionId, StatusEpoch};
use serde_json::Value;

const STATUS_EPOCH: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const OCCUPANT_ID: &str = "11111111-aaaa-4aaa-8aaa-111111111111";

/// The fence a transition that is still the current one runs under.
fn always_current() -> Arc<dyn Fn() -> bool + Send + Sync> {
    Arc::new(|| true)
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
async fn a_custom_title_wins_over_the_directory_leaf() {
    let fixture = PushFixture::new("dispatch-title").await;
    seed_open_session(&fixture, "/work/project").await;
    fixture
        .exec(&format!(
            "UPDATE sessions SET custom_title = 'Release run' WHERE id = '{}'",
            push_fixture::SESSION_ID
        ))
        .await;
    fixture
        .seed_subscription(
            &fixture.dashboard_id,
            &fixture.fp(),
            &format!("{PUSH_ORIGIN}/title"),
        )
        .await;
    let transport = push_fixture::FakeTransport::accepting();
    let current = always_current();

    fire_push_for_transition(
        fixture.database().pool(),
        &transition(PushTransition::Blocked, 1),
        &[PUSH_ORIGIN.to_owned()],
        &NoTerminalViewers,
        &current,
        transport.as_ref(),
    )
    .await;

    let payload: Value =
        serde_json::from_str(&transport.deliveries()[0].body).expect("the payload is JSON");
    assert_eq!(payload["title"], "Release run");
}

#[tokio::test]
async fn two_revisions_of_one_transition_get_different_deduplication_tokens() {
    // The RFC 8030 topic replaces an undelivered notification carrying the SAME
    // token. Two revisions are two facts, so a shared token would let the
    // second silently replace the first instead of arriving alongside it.
    let fixture = PushFixture::new("dispatch-token").await;
    seed_open_session(&fixture, "/work/project").await;
    fixture
        .seed_subscription(
            &fixture.dashboard_id,
            &fixture.fp(),
            &format!("{PUSH_ORIGIN}/token"),
        )
        .await;
    let transport = push_fixture::FakeTransport::accepting();
    let current = always_current();

    for revision in 1..=2 {
        fire_push_for_transition(
            fixture.database().pool(),
            &transition(PushTransition::Blocked, revision),
            &[PUSH_ORIGIN.to_owned()],
            &NoTerminalViewers,
            &current,
            transport.as_ref(),
        )
        .await;
    }

    let tokens: Vec<String> = transport
        .deliveries()
        .iter()
        .map(|delivery| {
            serde_json::from_str::<Value>(&delivery.body).expect("the payload is JSON")["deduplicationToken"]
                .as_str()
                .expect("a token")
                .to_owned()
        })
        .collect();
    assert_eq!(tokens.len(), 2);
    assert_ne!(
        tokens[0], tokens[1],
        "two revisions are two notifications, not one replaced"
    );
}
