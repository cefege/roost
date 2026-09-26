//! The admission order of the agent status hub: what a stale report may and may
//! not change, what a session close fences, and the one order a list and a
//! broadcast both answer with.
//!
//! These are the tests that decide whether a user is told the truth. A stale
//! frame that is applied instead of dropped is a dashboard that says an agent
//! is working when it finished a minute ago, and nothing in the product would
//! look broken -- so the fence is exercised directly, against the retained
//! table and against what went on the bus.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod agent_fixture;

use std::sync::{Arc, Mutex};

use agent_fixture::{
    AgentFixture, OCCUPANT_A, OCCUPANT_B, SESSION_IDS, SESSION_MISSING, WORKER_A, WORKER_B,
    legacy_status, session, status, worker,
};
use roost_coord::agents::status_hub::AgentStatusAcceptance;
use roost_protocol::wire::AgentStatusUpdate;
use serde_json::json;

/// Every update the hub published, in publication order.
type Published = Arc<Mutex<Vec<AgentStatusUpdate>>>;

fn subscribe(fixture: &AgentFixture) -> Published {
    let published: Published = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&published);
    fixture.core.services.buses.agent_status_bus.subscribe(move |update| {
        sink.lock()
            .expect("the publication sink")
            .push(update.clone());
    });
    published
}

fn retained(fixture: &AgentFixture) -> Vec<String> {
    fixture
        .hub()
        .snapshot()
        .into_iter()
        .map(|held| {
            format!(
                "{}:{}:{}",
                held.common.session_id.as_str(),
                held.common.occupant_id.as_ref().map_or("-", |id| id.as_str()),
                held.common.revision
            )
        })
        .collect()
}

fn accepted(
    fixture: &AgentFixture,
    worker_fp: &str,
    session_id: &str,
    overrides: serde_json::Value,
) -> AgentStatusAcceptance {
    let outcome = fixture.hub().accept_worker_status(
        &fixture.core,
        &worker(worker_fp),
        status(session_id, overrides),
    );
    assert_eq!(
        outcome,
        AgentStatusAcceptance::Accepted,
        "{session_id} {overrides} must be accepted"
    );
    outcome
}

#[tokio::test]
async fn a_late_report_never_displaces_a_fresh_one() {
    let fixture = AgentFixture::new("stale").await;
    accepted(&fixture, WORKER_A, SESSION_IDS[0], json!({"revision": 90}));
    let late = fixture.hub().accept_worker_status(
        &fixture.core,
        &worker(WORKER_A),
        status(SESSION_IDS[0], json!({"revision": 89, "state": "blocked"})),
    );
    assert_eq!(late, AgentStatusAcceptance::Stale);
    assert_eq!(
        retained(&fixture),
        vec![format!("{}:{OCCUPANT_A}:90", SESSION_IDS[0])],
        "the late report changed nothing"
    );

    // The same revision again is a reconnect resend, not progress.
    let resend = fixture.hub().accept_worker_status(
        &fixture.core,
        &worker(WORKER_A),
        status(SESSION_IDS[0], json!({"revision": 90})),
    );
    assert_eq!(resend, AgentStatusAcceptance::Stale);
    assert_eq!(
        retained(&fixture),
        vec![format!("{}:{OCCUPANT_A}:90", SESSION_IDS[0])],
        "a reconnect resend at the same revision is not progress either"
    );
}

#[tokio::test]
async fn a_replacement_occupant_is_not_a_stale_report_and_retires_the_previous_one() {
    let fixture = AgentFixture::new("replace").await;
    accepted(&fixture, WORKER_A, SESSION_IDS[0], json!({"revision": 90}));
    // A brand new agent numbers its revisions from 1: refusing it as stale
    // would strand the session with no agent at all.
    accepted(
        &fixture,
        WORKER_A,
        SESSION_IDS[0],
        json!({"revision": 1, "occupant_id": OCCUPANT_B}),
    );
    // The replaced occupant is fenced, however high its revision and whether
    // or not it is trying to delete the row.
    for overrides in [json!({"revision": 91}), json!({"revision": 92, "active": false})] {
        let refused = fixture.hub().accept_worker_status(
            &fixture.core,
            &worker(WORKER_A),
            status(SESSION_IDS[0], overrides),
        );
        assert_eq!(refused, AgentStatusAcceptance::Stale, "{overrides}");
    }
    let held = retained(&fixture);
    assert_eq!(held, vec![format!("{}:{}:1", SESSION_IDS[0], OCCUPANT_B)]);
}

#[tokio::test]
async fn an_inactive_update_deletes_the_row_and_keeps_its_revision_floor() {
    let fixture = AgentFixture::new("delete").await;
    accepted(&fixture, WORKER_A, SESSION_IDS[0], json!({"revision": 3}));
    accepted(
        &fixture,
        WORKER_A,
        SESSION_IDS[0],
        json!({"revision": 4, "active": false}),
    );
    assert!(retained(&fixture).is_empty());
    // The floor survives the deletion: a retried publish of revision 3 must not
    // resurrect a row the coordinator has already published as gone.
    let refused = fixture.hub().accept_worker_status(
        &fixture.core,
        &worker(WORKER_A),
        status(SESSION_IDS[0], json!({"revision": 3})),
    );
    assert_eq!(refused, AgentStatusAcceptance::Stale);
}

#[tokio::test]
async fn a_legacy_frame_yields_permanently_once_an_identified_occupant_is_accepted() {
    let fixture = AgentFixture::new("legacy").await;
    let legacy = fixture.hub().accept_worker_status(
        &fixture.core,
        &worker(WORKER_A),
        legacy_status(SESSION_IDS[0], json!({"revision": 40})),
    );
    assert_eq!(legacy, AgentStatusAcceptance::Accepted);
    accepted(&fixture, WORKER_A, SESSION_IDS[0], json!({"revision": 1}));
    for overrides in [
        json!({"revision": 100}),
        json!({"revision": 101, "active": false}),
    ] {
        let refused = fixture.hub().accept_worker_status(
            &fixture.core,
            &worker(WORKER_A),
            legacy_status(SESSION_IDS[0], overrides),
        );
        assert_eq!(refused, AgentStatusAcceptance::Stale, "{overrides}");
    }
    assert_eq!(
        retained(&fixture),
        vec![format!("{}:{OCCUPANT_A}:1", SESSION_IDS[0])],
        "the identified occupant is what is retained, whatever the legacy frame said"
    );
}

#[tokio::test]
async fn a_claim_from_a_worker_that_does_not_own_the_session_is_refused() {
    let fixture = AgentFixture::new("ownership").await;
    // Session three belongs to worker B, so worker A claiming it is refused even
    // though the payload is perfectly well formed.
    let refused = fixture.hub().accept_worker_status(
        &fixture.core,
        &worker(WORKER_A),
        status(SESSION_IDS[2], json!({})),
    );
    assert_eq!(refused, AgentStatusAcceptance::WrongWorker);
    // A session the coordinator has never heard of is refused differently,
    // because nothing was claimed at all.
    let unknown = fixture.hub().accept_worker_status(
        &fixture.core,
        &worker(WORKER_A),
        status(SESSION_MISSING, json!({})),
    );
    assert_eq!(unknown, AgentStatusAcceptance::UnknownSession);
    assert!(retained(&fixture).is_empty());
}

#[tokio::test]
async fn a_close_retires_the_occupant_and_a_reopen_does_not_unretire_it() {
    let fixture = AgentFixture::new("close").await;
    let published = subscribe(&fixture);
    accepted(&fixture, WORKER_A, SESSION_IDS[0], json!({"revision": 8}));
    fixture
        .hub()
        .note_session_closed(&fixture.core.services.buses, &session(SESSION_IDS[0]));

    let closing = published.lock().expect("the publication sink");
    assert_eq!(closing.len(), 2, "the accepted frame and the synthetic close");
    let synthetic = closing.last().expect("the close publication");
    assert!(!synthetic.active, "a close publishes a deletion");
    assert_eq!(synthetic.common.revision, 9, "one past the last revision");
    assert_eq!(
        synthetic.common.occupant_id.as_ref().map(|id| id.as_str()),
        Some(OCCUPANT_A),
        "the close names the occupant it retired"
    );
    drop(closing);

    let refused = fixture.hub().accept_worker_status(
        &fixture.core,
        &worker(WORKER_A),
        status(SESSION_IDS[0], json!({"revision": 9})),
    );
    assert_eq!(refused, AgentStatusAcceptance::Stale, "closed is fenced");

    fixture
        .hub()
        .note_session_opened(&session(SESSION_IDS[0]));
    accepted(
        &fixture,
        WORKER_A,
        SESSION_IDS[0],
        json!({"revision": 0, "occupant_id": OCCUPANT_B}),
    );
    // The reopen drops the close fence but not the retirement: the old
    // occupant's next reconnect must not be able to overwrite the new one.
    for overrides in [
        json!({"revision": 100}),
        json!({"revision": 101, "active": false}),
    ] {
        let refused = fixture.hub().accept_worker_status(
            &fixture.core,
            &worker(WORKER_A),
            status(SESSION_IDS[0], overrides),
        );
        assert_eq!(refused, AgentStatusAcceptance::Stale, "{overrides}");
    }
    assert_eq!(
        retained(&fixture),
        vec![format!("{}:{}:0", SESSION_IDS[0], OCCUPANT_B)]
    );
}

#[tokio::test]
async fn the_list_order_and_the_broadcast_order_are_one_answer_after_a_reordering() {
    let fixture = AgentFixture::new("order").await;
    let published = subscribe(&fixture);
    // Insert in an order that is deliberately not the answer's order.
    for (index, session_id) in SESSION_IDS.iter().enumerate() {
        let worker_fp = if index == 2 { WORKER_B } else { WORKER_A };
        accepted(
            &fixture,
            worker_fp,
            session_id,
            json!({"revision": i64::try_from(index).expect("an index") + 1}),
        );
    }
    let sorted: Vec<String> = [SESSION_IDS[1], SESSION_IDS[3], SESSION_IDS[0], SESSION_IDS[2]]
        .iter()
        .map(|id| (*id).to_owned())
        .collect();
    assert_eq!(
        fixture
            .hub()
            .snapshot()
            .iter()
            .map(|held| held.common.session_id.to_string())
            .collect::<Vec<String>>(),
        sorted,
        "the retained table answers in session-id order, whatever order it was filled in"
    );

    // A client that applied the broadcasts must now hold exactly what a client
    // that re-fetched holds.
    assert_eq!(fold_publications(&published), retained_rows(&fixture));

    // Reorder: session two is replaced by a new occupant, and session four goes
    // away entirely. The two answers must still be the same answer.
    accepted(
        &fixture,
        WORKER_A,
        SESSION_IDS[3],
        json!({"revision": 2, "occupant_id": OCCUPANT_B, "state": "blocked"}),
    );
    accepted(
        &fixture,
        WORKER_A,
        SESSION_IDS[1],
        json!({"revision": 3, "active": false}),
    );
    assert_eq!(fold_publications(&published), retained_rows(&fixture));
    assert_eq!(
        fixture
            .hub()
            .snapshot()
            .iter()
            .map(|held| held.common.session_id.to_string())
            .collect::<Vec<String>>(),
        vec![
            SESSION_IDS[0].to_owned(),
            SESSION_IDS[2].to_owned(),
            SESSION_IDS[3].to_owned()
        ],
        "a deletion removes the row, and the rest keep their order"
    );
}

/// What a browser holds after applying every broadcast, in session order.
fn fold_publications(published: &Published) -> Vec<String> {
    let mut applied: std::collections::BTreeMap<String, String> = Default::default();
    for update in published.lock().expect("the publication sink").iter() {
        let session_id = update.common.session_id.to_string();
        if update.active {
            applied.insert(
                session_id.clone(),
                format!(
                    "{}:{}",
                    update
                        .common
                        .occupant_id
                        .as_ref()
                        .map_or("-", |id| id.as_str()),
                    update.common.revision
                ),
            );
        } else {
            applied.remove(&session_id);
        }
    }
    applied
        .into_iter()
        .map(|(session_id, row)| format!("{session_id}:{row}"))
        .collect()
}

/// What the hub holds, rendered the same way.
fn retained_rows(fixture: &AgentFixture) -> Vec<String> {
    retained(fixture)
}
