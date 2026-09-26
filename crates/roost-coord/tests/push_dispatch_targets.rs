//! Per-device push dispatch for an agent transition: who gets told, who is
//! suppressed, and the identity-derived deduplication token.
//!
//! The viewer seam is a trait parameter here rather than a call into the
//! terminal domain, which does not exist yet. `NoTerminalViewers` is the value
//! a caller passes until it does, and it means "nobody is viewing".

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod push_fixture;

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use push_fixture::{PUSH_ORIGIN, PushFixture, seed_open_session, viewer_fp};
use roost_coord::push::dispatch::{
    ActiveTerminalViewers, AgentPushTransition, NoTerminalViewers, PushTransition,
    fire_push_for_transition,
};
use roost_coord::push::vapid::P256KeypairGenerator;
use roost_protocol::wire::{AgentOccupantId, SessionId, StatusEpoch};
use serde_json::Value;

const STATUS_EPOCH: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const OCCUPANT_ID: &str = "11111111-aaaa-4aaa-8aaa-111111111111";

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

/// The fence a transition that is still the current one runs under.
fn always_current() -> Arc<dyn Fn() -> bool + Send + Sync> {
    Arc::new(|| true)
}

/// The fence a transition that has been superseded runs under.
fn never_current() -> Arc<dyn Fn() -> bool + Send + Sync> {
    Arc::new(|| false)
}

/// A viewer set that always answers with `fingerprints`.
struct FixedViewers(HashSet<String>);

impl ActiveTerminalViewers for FixedViewers {
    fn active_viewer_fingerprints(&self, _session_id: &str) -> HashSet<String> {
        self.0.clone()
    }
}

/// Add a second browser device, so a dispatch has more than one candidate.
async fn seed_second_device(fixture: &PushFixture, fp: &str) {
    sqlx::query(
        "INSERT INTO authorized_keys (fingerprint, public_key, label, added_at) \
         VALUES (?1, ?2, 'second', 1000)",
    )
    .bind(fp)
    .bind(vec![1_u8; 32])
    .execute(fixture.database().pool())
    .await
    .expect("the second device key");
    sqlx::query(
        "INSERT INTO account_devices (fingerprint, account_id, added_at_ms, last_seen_at_ms) \
         VALUES (?1, ?2, 1000, 1000)",
    )
    .bind(fp)
    .bind(&fixture.account_id)
    .execute(fixture.database().pool())
    .await
    .expect("the second account device");
}

#[tokio::test]
async fn a_transition_notifies_every_background_device_with_the_full_payload() {
    let fixture = PushFixture::new("dispatch-basic").await;
    seed_open_session(&fixture, "/work/project").await;
    let background = viewer_fp('b');
    seed_second_device(&fixture, &background).await;
    fixture
        .seed_subscription(
            &fixture.dashboard_id,
            &background,
            &format!("{PUSH_ORIGIN}/background"),
        )
        .await;
    let transport = push_fixture::FakeTransport::accepting();
    let current = always_current();

    fire_push_for_transition(
        fixture.database().pool(),
        &transition(PushTransition::Blocked, 7),
        &[PUSH_ORIGIN.to_owned()],
        &NoTerminalViewers,
        &current,
        transport.as_ref(),
    )
    .await;

    let deliveries = transport.deliveries();
    assert_eq!(deliveries.len(), 1, "one background device, one delivery");
    let payload: Value = serde_json::from_str(&deliveries[0].body).expect("the payload is JSON");
    assert_eq!(payload["sessionId"], push_fixture::SESSION_ID);
    assert_eq!(payload["kind"], "blocked");
    assert_eq!(payload["title"], "project", "the leaf of the session's cwd");
    assert_eq!(payload["body"], "Needs your input");
    assert_eq!(payload["statusEpoch"], STATUS_EPOCH);
    assert_eq!(payload["occupantId"], OCCUPANT_ID);
    assert_eq!(payload["revision"], 7);
    let token = payload["deduplicationToken"]
        .as_str()
        .expect("a deduplication token");
    assert_eq!(token.len(), 32, "32 base64url characters");
    assert!(
        token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')),
        "the topic is base64url: {token}"
    );
    assert_eq!(
        deliveries[0].topic.as_deref(),
        Some(token),
        "the topic and the payload's token are the same value"
    );
}

#[tokio::test]
async fn a_done_transition_says_finished_rather_than_needs_your_input() {
    let fixture = PushFixture::new("dispatch-done").await;
    seed_open_session(&fixture, "/work/project").await;
    fixture
        .seed_subscription(
            &fixture.dashboard_id,
            &fixture.fp(),
            &format!("{PUSH_ORIGIN}/done"),
        )
        .await;
    let transport = push_fixture::FakeTransport::accepting();
    let current = always_current();

    fire_push_for_transition(
        fixture.database().pool(),
        &transition(PushTransition::Done, 1),
        &[PUSH_ORIGIN.to_owned()],
        &NoTerminalViewers,
        &current,
        transport.as_ref(),
    )
    .await;

    let deliveries = transport.deliveries();
    let payload: Value = serde_json::from_str(&deliveries[0].body).expect("the payload is JSON");
    assert_eq!(payload["kind"], "done");
    assert_eq!(payload["body"], "Finished");
}

#[tokio::test]
async fn a_device_already_watching_the_session_is_not_notified() {
    let fixture = PushFixture::new("dispatch-suppress").await;
    seed_open_session(&fixture, "/work/project").await;
    let watching = fixture.fp();
    let background = viewer_fp('b');
    seed_second_device(&fixture, &background).await;
    for fp in [&watching, &background] {
        fixture
            .seed_subscription(
                &fixture.dashboard_id,
                fp,
                &format!("{PUSH_ORIGIN}/sub-{fp}"),
            )
            .await;
    }
    let transport = push_fixture::FakeTransport::accepting();
    let viewers = FixedViewers(HashSet::from([watching.clone()]));

    fire_push_for_transition(
        fixture.database().pool(),
        &transition(PushTransition::Blocked, 7),
        &[PUSH_ORIGIN.to_owned()],
        &viewers,
        &current,
        transport.as_ref(),
    )
    .await;

    let deliveries = transport.deliveries();
    assert_eq!(
        deliveries.len(),
        1,
        "the device looking at the terminal is not also told about it"
    );
    assert!(
        deliveries[0].endpoint.contains(&background),
        "and the delivery went to the background device"
    );
}

#[tokio::test]
async fn a_device_removed_from_the_registry_has_its_row_pruned_and_is_not_told() {
    let fixture = PushFixture::new("dispatch-orphan").await;
    seed_open_session(&fixture, "/work/project").await;
    let gone = viewer_fp('b');
    seed_second_device(&fixture, &gone).await;
    fixture
        .seed_subscription(
            &fixture.dashboard_id,
            &gone,
            &format!("{PUSH_ORIGIN}/orphaned"),
        )
        .await;
    fixture
        .seed_subscription(
            &fixture.dashboard_id,
            &fixture.fp(),
            &format!("{PUSH_ORIGIN}/kept"),
        )
        .await;
    // Only the `account_devices` association goes; the push row is left behind,
    // which is exactly the legacy-cleanup shape the prune exists to repair.
    fixture
        .exec(&format!(
            "DELETE FROM account_devices WHERE fingerprint = '{gone}'"
        ))
        .await;
    let transport = push_fixture::FakeTransport::accepting();
    let current = always_current();

    fire_push_for_transition(
        fixture.database().pool(),
        &transition(PushTransition::Done, 8),
        &[PUSH_ORIGIN.to_owned()],
        &NoTerminalViewers,
        &current,
        transport.as_ref(),
    )
    .await;

    assert_eq!(
        transport.deliveries().len(),
        1,
        "the row whose device is gone is not a delivery target"
    );
    assert!(
        fixture.endpoints_for(&gone).await.is_empty(),
        "and dispatch repairs the row rather than leaving it to accumulate"
    );
}

#[tokio::test]
async fn a_disabled_account_keeps_its_rows_but_receives_nothing() {
    let fixture = PushFixture::new("dispatch-disabled").await;
    seed_open_session(&fixture, "/work/project").await;
    fixture
        .seed_subscription(
            &fixture.dashboard_id,
            &fixture.fp(),
            &format!("{PUSH_ORIGIN}/disabled"),
        )
        .await;
    fixture
        .exec(&format!(
            "UPDATE accounts SET status = 'disabled' WHERE id = '{}'",
            fixture.account_id
        ))
        .await;
    let transport = push_fixture::FakeTransport::accepting();
    let current = always_current();

    fire_push_for_transition(
        fixture.database().pool(),
        &transition(PushTransition::Done, 9),
        &[PUSH_ORIGIN.to_owned()],
        &NoTerminalViewers,
        &current,
        transport.as_ref(),
    )
    .await;

    assert!(
        transport.deliveries().is_empty(),
        "a disabled account is not told"
    );
    assert_eq!(
        fixture.endpoints_for(&fixture.fp()).await.len(),
        1,
        "but its subscription is retained, so re-enabling needs no re-subscribe"
    );
}

#[tokio::test]
async fn a_subscription_whose_origin_left_the_allowlist_is_dropped_not_delivered() {
    let fixture = PushFixture::new("dispatch-deconfigured").await;
    seed_open_session(&fixture, "/work/project").await;
    let deconfigured = viewer_fp('b');
    seed_second_device(&fixture, &deconfigured).await;
    fixture
        .seed_subscription(
            &fixture.dashboard_id,
            &deconfigured,
            "https://retired.example/subscription",
        )
        .await;
    let transport = push_fixture::FakeTransport::accepting();

    // The operator removed `retired.example` from the allowlist after this row
    // was written. Re-validating at send time is the whole reason the dispatch
    // filters: a subscription that predates the change must not keep receiving.
    fire_push_for_transition(
        fixture.database().pool(),
        &transition(PushTransition::Blocked, 1),
        &[PUSH_ORIGIN.to_owned()],
        &NoTerminalViewers,
        &current,
        transport.as_ref(),
    )
    .await;

    assert!(transport.deliveries().is_empty());
    assert_eq!(
        fixture.endpoints_for(&deconfigured).await.len(),
        1,
        "the row is left in place; only the delivery is withheld"
    );
}

#[tokio::test]
async fn a_closed_session_produces_no_notification() {
    let fixture = PushFixture::new("dispatch-closed").await;
    seed_open_session(&fixture, "/work/project").await;
    fixture
        .seed_subscription(
            &fixture.dashboard_id,
            &fixture.fp(),
            &format!("{PUSH_ORIGIN}/closed"),
        )
        .await;
    fixture
        .exec(&format!(
            "UPDATE sessions SET status = 'closed', closed_at = 2000 WHERE id = '{}'",
            push_fixture::SESSION_ID
        ))
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

    assert!(
        transport.deliveries().is_empty(),
        "a session closed between the transition and the query must not be announced"
    );
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
    let _ = (
        Arc::new(AtomicUsize::new(0)),
        P256KeypairGenerator,
        Ordering::SeqCst,
    );
}
