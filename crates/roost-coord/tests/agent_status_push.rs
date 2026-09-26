//! The push schedule: which accepted agent transitions are worth waking a phone
//! for, when, and what a superseded schedule does instead.
//!
//! The threshold is the interesting part. Everything else about agent status is
//! already broadcast to every connected client the instant it is accepted, so a
//! push is only justified for a change the user is not looking at -- an agent
//! that needs a human, and a turn that finished.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod agent_fixture;

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_fixture::{
    AgentFixture, OCCUPANT_B, OCCUPANT_C, SESSION_IDS, WORKER_A, session, status, worker,
};
use roost_coord::agents::status_push::{AgentStatusPushDelivery, PushTransitions};
use roost_coord::push::dispatch::{
    AgentPushTransition, NoTerminalViewers, PushDeliveryRequest, PushNotificationTransport,
    PushTransportError,
};
use serde_json::{Value, json};

/// Long enough that a delivery which is going to happen has not yet, short
/// enough that a test does not wait a real second for it.
const DEBOUNCE: Duration = Duration::from_millis(40);
/// The margin that must pass with nothing delivered.
const BEFORE_DEBOUNCE: Duration = Duration::from_millis(10);

type Fence = Arc<dyn Fn() -> bool + Send + Sync>;

/// A delivery that records what it was asked to send, and keeps the fence so a
/// test can ask it again later.
#[derive(Default)]
struct RecordingDelivery {
    enabled: bool,
    sent: Mutex<Vec<(AgentPushTransition, Fence)>>,
}

impl AgentStatusPushDelivery for RecordingDelivery {
    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn deliver(&self, transition: &AgentPushTransition, is_current: Fence) {
        self.sent
            .lock()
            .expect("the delivery sink")
            .push((transition.clone(), is_current));
    }
}

impl RecordingDelivery {
    fn notified(&self) -> Vec<(&'static str, u64, String)> {
        self.sent
            .lock()
            .expect("the delivery sink")
            .iter()
            .map(|(transition, _)| {
                (
                    transition.kind.as_str(),
                    transition.revision,
                    transition.occupant_id.as_str().to_owned(),
                )
            })
            .collect()
    }

    fn fence(&self, index: usize) -> Fence {
        self.sent
            .lock()
            .expect("the delivery sink")
            .get(index)
            .map(|(_, fence)| Arc::clone(fence))
            .expect("a recorded delivery")
    }
}

async fn fixture_with_delivery(label: &str, enabled: bool) -> (AgentFixture, Arc<RecordingDelivery>) {
    let fixture = AgentFixture::build(label, Arc::new(roost_coord::serve::now_ms), DEBOUNCE).await;
    let delivery = Arc::new(RecordingDelivery {
        enabled,
        sent: Mutex::new(Vec::new()),
    });
    fixture
        .hub()
        .install_push_delivery(Arc::clone(&delivery) as Arc<dyn AgentStatusPushDelivery>);
    (fixture, delivery)
}

/// Feed one accepted frame. Every step of a push scenario is a report the hub
/// must accept, so a refused one is a failure here rather than a confusing
/// missing notification three steps later.
fn retain(fixture: &AgentFixture, overrides: Value) {
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
async fn only_a_block_and_a_finished_turn_reach_a_phone() {
    let (fixture, delivery) = fixture_with_delivery("push-threshold", true).await;
    retain(&fixture, json!({"revision": 1, "state": "working"}));
    // Still working, with a new message: already on every connected screen, and
    // nothing a person who is not looking would act on.
    retain(
        &fixture,
        json!({"revision": 2, "state": "working", "message": "editing"}),
    );
    tokio::time::sleep(DEBOUNCE * 3).await;
    assert_eq!(
        delivery.notified(),
        Vec::new(),
        "progress is not a notification"
    );

    retain(
        &fixture,
        json!({"revision": 3, "state": "blocked", "message": "Approval needed"}),
    );
    tokio::time::sleep(BEFORE_DEBOUNCE).await;
    assert_eq!(
        delivery.notified(),
        Vec::new(),
        "the debounce is a real window, not a delay of zero"
    );
    tokio::time::sleep(DEBOUNCE * 3).await;
    assert_eq!(delivery.notified().len(), 1);
    assert_eq!(delivery.notified()[0].0, "blocked");
    assert_eq!(delivery.notified()[0].1, 3);

    // Working again, then going idle without a completed turn: the agent gave
    // up or was interrupted, and no turn finished, so nothing is worth waking
    // anyone for.
    retain(&fixture, json!({"revision": 4, "state": "working"}));
    retain(
        &fixture,
        json!({"revision": 5, "state": "idle", "completed_revision": 0}),
    );
    tokio::time::sleep(DEBOUNCE * 3).await;
    assert_eq!(delivery.notified().len(), 1, "no turn completed");

    retain(&fixture, json!({"revision": 6, "state": "blocked"}));
    tokio::time::sleep(DEBOUNCE * 3).await;
    assert_eq!(delivery.notified().len(), 2);
    assert_eq!(delivery.notified()[1].0, "blocked");
    retain(
        &fixture,
        json!({"revision": 7, "state": "idle", "completed_revision": 7}),
    );
    tokio::time::sleep(DEBOUNCE * 3).await;
    assert_eq!(delivery.notified().len(), 3);
    assert_eq!(delivery.notified()[2].0, "done");
    assert_eq!(delivery.notified()[2].1, 7);
}

#[tokio::test]
async fn a_same_state_republish_keeps_the_revision_that_caused_the_notification() {
    let (fixture, delivery) = fixture_with_delivery("push-carry", true).await;
    retain(&fixture, json!({"revision": 1, "state": "working"}));
    retain(&fixture, json!({"revision": 2, "state": "blocked"}));
    // The agent is still blocked; only the authority source moved. The
    // notification must still name the revision that caused it.
    retain(
        &fixture,
        json!({"revision": 3, "state": "blocked", "source": "screen"}),
    );
    tokio::time::sleep(DEBOUNCE * 3).await;
    assert_eq!(delivery.notified()[0].1, 2, "the carried trigger revision");
}

#[tokio::test]
async fn a_replacement_occupant_neither_inherits_nor_triggers_the_previous_notification() {
    let (fixture, delivery) = fixture_with_delivery("push-replace", true).await;
    retain(&fixture, json!({"revision": 1, "state": "working"}));
    retain(&fixture, json!({"revision": 2, "state": "blocked"}));
    // The agent went back to work, so the armed block is cancelled rather than
    // delivered: the phone would be told about a question nobody has any more.
    retain(&fixture, json!({"revision": 3, "state": "working"}));
    tokio::time::sleep(DEBOUNCE * 3).await;
    assert_eq!(
        delivery.notified(),
        Vec::new(),
        "a cancelled block is not sent late"
    );

    // A replacement occupant has no state of its own to transition FROM, so its
    // first report is never a notification -- and it does not inherit the
    // occupant it replaced.
    retain(
        &fixture,
        json!({"revision": 1, "occupant_id": OCCUPANT_B, "state": "blocked"}),
    );
    tokio::time::sleep(DEBOUNCE * 3).await;
    assert_eq!(
        delivery.notified(),
        Vec::new(),
        "a replacement's first report is a fact, not a transition"
    );
    // Its own working -> blocked, though, is a real transition, and it carries
    // its own deduplication token so a later delivery replaces rather than
    // stacks behind the previous occupant's.
    retain(
        &fixture,
        json!({"revision": 2, "occupant_id": OCCUPANT_B, "state": "working"}),
    );
    retain(
        &fixture,
        json!({"revision": 3, "occupant_id": OCCUPANT_B, "state": "blocked"}),
    );
    tokio::time::sleep(DEBOUNCE * 3).await;
    assert_eq!(delivery.notified().len(), 1);
    assert_eq!(delivery.notified()[0].2, OCCUPANT_B);

    retain(
        &fixture,
        json!({"revision": 1, "occupant_id": OCCUPANT_C, "state": "working"}),
    );
    retain(
        &fixture,
        json!({"revision": 2, "occupant_id": OCCUPANT_C, "state": "blocked"}),
    );
    tokio::time::sleep(DEBOUNCE * 3).await;
    let notified = delivery.notified();
    assert_eq!(notified.len(), 2, "one notification per occupant turn");
    assert_eq!(notified[1].2, OCCUPANT_C);
}

#[tokio::test]
async fn a_status_that_moved_on_before_the_debounce_ends_sends_nothing() {
    let (fixture, delivery) = fixture_with_delivery("push-fence", true).await;
    retain(&fixture, json!({"revision": 1, "state": "working"}));
    retain(&fixture, json!({"revision": 2, "state": "blocked"}));
    // The row is superseded without a state the schedule can carry, so the
    // armed arm no longer describes what the session is doing.
    retain(
        &fixture,
        json!({"revision": 3, "state": "blocked", "source": "screen", "active": false}),
    );
    tokio::time::sleep(DEBOUNCE * 3).await;
    assert_eq!(delivery.notified(), Vec::new(), "a deleted row notifies nobody");
}

#[tokio::test]
async fn a_delivered_notification_carries_a_fence_that_later_turns_false() {
    let (fixture, delivery) = fixture_with_delivery("push-current", true).await;
    retain(&fixture, json!({"revision": 1, "state": "working"}));
    retain(&fixture, json!({"revision": 2, "state": "blocked"}));
    tokio::time::sleep(DEBOUNCE * 3).await;
    assert_eq!(delivery.notified().len(), 1);
    let fence = delivery.fence(0);
    assert!(fence(), "the transition is current when it is handed over");

    // The same fence, asked again after the agent moved on, must now say no.
    // That is what stops a delivery whose database work outlived the state it
    // was about to describe.
    retain(&fixture, json!({"revision": 3, "state": "working"}));
    assert!(!fence(), "a superseded transition stops being current");
    assert_eq!(delivery.notified().len(), 1, "and is not sent twice");
}

#[tokio::test]
async fn a_session_that_closes_before_the_debounce_ends_sends_nothing() {
    let (fixture, delivery) = fixture_with_delivery("push-close", true).await;
    retain(&fixture, json!({"revision": 1, "state": "working"}));
    retain(&fixture, json!({"revision": 2, "state": "blocked"}));
    fixture
        .hub()
        .note_session_closed(&fixture.core.services.buses, &session(SESSION_IDS[0]));
    tokio::time::sleep(DEBOUNCE * 3).await;
    assert_eq!(
        delivery.notified(),
        Vec::new(),
        "a closed session has nothing left to be told about"
    );
}

#[tokio::test]
async fn a_coordinator_with_push_switched_off_arms_nothing() {
    let (fixture, delivery) = fixture_with_delivery("push-off", false).await;
    retain(&fixture, json!({"revision": 1, "state": "working"}));
    retain(&fixture, json!({"revision": 2, "state": "blocked"}));
    tokio::time::sleep(DEBOUNCE * 3).await;
    assert_eq!(delivery.notified(), Vec::new());
}

#[tokio::test]
async fn an_empty_operator_allowlist_is_push_switched_off() {
    let fixture = AgentFixture::new("push-allowlist").await;
    let switched_off = PushTransitions::new(
        fixture.database().pool().clone(),
        Vec::new(),
        Arc::new(NoTerminalViewers),
        Arc::new(SilentTransport),
    );
    assert!(!switched_off.is_enabled());
    let allowed = PushTransitions::new(
        fixture.database().pool().clone(),
        vec!["https://push.example".to_owned()],
        Arc::new(NoTerminalViewers),
        Arc::new(SilentTransport),
    );
    assert!(allowed.is_enabled());
}

/// A transport that reaches nothing, for the allowlist question only.
struct SilentTransport;

impl PushNotificationTransport for SilentTransport {
    fn send<'a>(
        &'a self,
        _request: &'a PushDeliveryRequest,
    ) -> Pin<Box<dyn Future<Output = Result<(), PushTransportError>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}
