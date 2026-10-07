//! An agent transition, accepted by the status hub, reaching a subscribed
//! device through the production delivery (`PushTransitions` over the
//! coordinator's real `TerminalViewHub`), with a recording transport at the
//! network edge.
//!
//! Ports "suppresses only devices actively viewing the session" from
//! `apps/coord/tests/push/push-delivery-sender.test.ts`, which drives v2's real
//! terminal view hub (`push-dispatch.ts:92`) rather than a viewer double. v2's
//! push threshold is the working -> blocked EDGE
//! (`agent-status-push-scheduler.ts:98`); idle -> blocked is not a push, which
//! `agent_status_push.rs` pins. The last case registers its device through
//! `PushSubscribe`/`PushUnsubscribe`, exactly as the web client's Desktop
//! switch does, so the row the RPC writes is proven to be the row the
//! dispatch reads.
//!
//! `unwrap` and `expect` are denied outside `#[cfg(test)]`, and an integration
//! test is its own crate rather than a module of one, so the exemption has to
//! be stated here. Every panic names a value the test just built.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod agent_fixture;
mod db_support;
mod push_fixture;

use std::sync::Arc;
use std::time::Duration;

use agent_fixture::{AgentFixture, SESSION_IDS, WORKER_A, status, worker};
use push_fixture::{
    FakeTransport, PUSH_ORIGIN, RecordedDelivery, browser_caller, seed_device, viewer_fp,
};
use roost_coord::agents::status_hub::AgentStatusAcceptance;
use roost_coord::agents::status_push::PushTransitions;
use roost_coord::coord_core::CoordCore;
use roost_coord::push::PushRuntime;
use roost_coord::push::dispatch::ActiveTerminalViewers;
use roost_coord::push::rpc::{
    handle_push_get_config, handle_push_subscribe, handle_push_unsubscribe,
};
use roost_coord::terminal_view::{NoTerminalViewSink, SocketRegistration};
use roost_proto::{PushGetConfigRequest, PushSubscribeRequest, PushUnsubscribeRequest};
use serde_json::{Value, json};

/// The push debounce the hub runs under, short so the test does not wait v2's
/// real second.
const DEBOUNCE: Duration = Duration::from_millis(20);
/// A view id, which must be a uuid.
const VIEW: &str = "2b3c4d5e-6f70-4182-8c9d-1e2f3a4b5c6d";
/// The socket the viewing device holds.
const SOCKET: &str = "push-viewing-socket";

/// A hub whose push delivery is the production `PushTransitions`, over the
/// coordinator's own view hub and a transport that records.
async fn delivering(label: &str) -> (AgentFixture, Arc<FakeTransport>) {
    let fixture = AgentFixture::build(label, Arc::new(roost_coord::serve::now_ms), DEBOUNCE).await;
    let transport = FakeTransport::accepting();
    fixture
        .hub()
        .install_push_delivery(Arc::new(PushTransitions::new(
            fixture.database().pool().clone(),
            vec![PUSH_ORIGIN.to_owned()],
            Arc::clone(&fixture.core.services.views) as Arc<dyn ActiveTerminalViewers>,
            transport.clone(),
        )));
    (fixture, transport)
}

/// A device of the fixture's account, subscribed at `endpoint`.
async fn subscribed_device(fixture: &AgentFixture, fp: &str, endpoint: &str) {
    sqlx::query(
        "INSERT INTO authorized_keys (fingerprint, public_key, label, added_at) \
         VALUES ($1, $2, 'push-device', 1000)",
    )
    .bind(fp)
    .bind(vec![0_u8; 32])
    .execute(fixture.database().pool())
    .await
    .expect("the key row");
    sqlx::query(
        "INSERT INTO account_devices (fingerprint, account_id, added_at_ms, last_seen_at_ms) \
         VALUES ($1, $2, 1000, 1000)",
    )
    .bind(fp)
    .bind(&fixture.account_id)
    .execute(fixture.database().pool())
    .await
    .expect("the device row");
    sqlx::query(
        "INSERT INTO push_subscriptions \
           (dashboard_id, viewer_fp, endpoint, p256dh, auth, created_at_ms) \
         VALUES ($1, $2, $3, 'abc', 'def', 1000)",
    )
    .bind(&fixture.dashboard_id)
    .bind(fp)
    .bind(endpoint)
    .execute(fixture.database().pool())
    .await
    .expect("the subscription row");
}

/// `fp` holds a Sync socket with a live view of the session, as a browser
/// showing the terminal does.
fn viewing(fixture: &AgentFixture, fp: &str) {
    let views = &fixture.core.services.views;
    views.register_socket(
        &SocketRegistration {
            socket_id: SOCKET.to_owned(),
            viewer_key: Some(format!("{fp}:push-delivery-tab")),
            caller_fingerprint: fp.to_owned(),
            session_ids: [SESSION_IDS[0].to_owned()].into_iter().collect(),
            sink: Arc::new(NoTerminalViewSink),
        },
        0,
    );
    views.handle_view_command(
        SOCKET,
        &roost_proto::TerminalViewCommand {
            view_id: VIEW.to_owned(),
            session_id: SESSION_IDS[0].to_owned(),
            cols: 80,
            rows: 24,
            revision: 1,
            active: true,
            domain_generation: 1,
            __buffa_unknown_fields: Default::default(),
        },
        0,
    );
}

fn retain(fixture: &AgentFixture, overrides: Value) {
    let accepted = fixture.hub().accept_worker_status(
        &fixture.core,
        &worker(WORKER_A),
        status(SESSION_IDS[0], overrides.clone()),
    );
    assert_eq!(accepted, AgentStatusAcceptance::Accepted, "{overrides}");
}

/// The deliveries once `count` have landed and a further margin brought no
/// more. Bounded, so a delivery that never comes fails rather than hangs.
async fn settled(transport: &FakeTransport, count: usize) -> Vec<RecordedDelivery> {
    for _ in 0..300 {
        if transport.attempted() >= count {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(DEBOUNCE * 5).await;
    transport.deliveries()
}

fn endpoints(deliveries: &[RecordedDelivery]) -> Vec<String> {
    let mut endpoints: Vec<String> = deliveries.iter().map(|d| d.endpoint.clone()).collect();
    endpoints.sort();
    endpoints
}

#[tokio::test]
async fn a_blocked_agent_notifies_every_subscribed_device_not_viewing_the_session() {
    let (fixture, transport) = delivering("push-e2e").await;
    let watcher = viewer_fp('d');
    let background = viewer_fp('e');
    let watcher_endpoint = format!("{PUSH_ORIGIN}/viewing");
    let background_endpoint = format!("{PUSH_ORIGIN}/background");
    subscribed_device(&fixture, &watcher, &watcher_endpoint).await;
    subscribed_device(&fixture, &background, &background_endpoint).await;
    viewing(&fixture, &watcher);

    retain(&fixture, json!({"revision": 1, "state": "working"}));
    retain(&fixture, json!({"revision": 2, "state": "blocked"}));

    let deliveries = settled(&transport, 1).await;
    assert_eq!(
        endpoints(&deliveries),
        vec![background_endpoint.clone()],
        "one push, to the device that is not already looking at the terminal"
    );
    let payload: Value = serde_json::from_str(&deliveries[0].body).expect("a JSON payload");
    assert_eq!(payload["sessionId"], SESSION_IDS[0]);
    assert_eq!(payload["kind"], "blocked");
    assert_eq!(payload["body"], "Needs your input");
    assert_eq!(payload["revision"], 2);
    assert_eq!(payload["title"], "tmp");

    // The watcher stops watching: the next block reaches both devices, so the
    // suppression above was the view and nothing else about that device.
    fixture.core.services.views.remove_fingerprint(&watcher, 0);
    retain(&fixture, json!({"revision": 3, "state": "working"}));
    retain(&fixture, json!({"revision": 4, "state": "blocked"}));
    let deliveries = settled(&transport, 3).await;
    assert_eq!(
        endpoints(&deliveries[1..]),
        vec![background_endpoint, watcher_endpoint]
    );
}

#[tokio::test]
async fn a_needs_input_transition_pushes_to_the_subscription_the_browser_registered() {
    let (fixture, transport) = delivering("push-rpc-e2e").await;
    // The same services with the push surface installed, as `serve.rs` builds.
    let push_core = CoordCore::with_push(
        Arc::clone(&fixture.core.services),
        PushRuntime::new(fixture.dashboard_id.clone(), vec![PUSH_ORIGIN.to_owned()]),
    );
    let device = viewer_fp('f');
    seed_device(fixture.database(), &fixture.account_id, &device).await;
    let caller = browser_caller(&device, &fixture.account_id);

    let config = handle_push_get_config(&push_core, &caller, PushGetConfigRequest::default())
        .await
        .expect("a paired browser reads the push config");
    assert!(config.body.available);
    assert!(!config.body.vapid_public_key_b64.is_empty());
    let endpoint = format!("{PUSH_ORIGIN}/desktop-switch");
    let subscribed = handle_push_subscribe(
        &push_core,
        &caller,
        PushSubscribeRequest {
            endpoint: endpoint.clone(),
            p256dh: "BPk-browser_key".to_owned(),
            auth: "c2VjcmV0".to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect("the browser's subscription is admitted");
    assert!(subscribed.body.ok);

    retain(&fixture, json!({"revision": 1, "state": "working"}));
    retain(&fixture, json!({"revision": 2, "state": "blocked"}));
    let deliveries = settled(&transport, 1).await;
    assert_eq!(endpoints(&deliveries), vec![endpoint.clone()]);
    let payload: Value = serde_json::from_str(&deliveries[0].body).expect("a JSON payload");
    assert_eq!(payload["sessionId"], SESSION_IDS[0]);
    assert_eq!(payload["kind"], "blocked");
    assert_eq!(payload["body"], "Needs your input");

    // Turning the switch off removes the row the dispatch reads.
    let unsubscribed = handle_push_unsubscribe(
        &push_core,
        &caller,
        PushUnsubscribeRequest {
            endpoint,
            ..Default::default()
        },
    )
    .await
    .expect("the browser's unsubscribe is admitted");
    assert!(unsubscribed.body.ok);
    retain(&fixture, json!({"revision": 3, "state": "working"}));
    retain(&fixture, json!({"revision": 4, "state": "blocked"}));
    tokio::time::sleep(DEBOUNCE * 10).await;
    assert_eq!(
        transport.attempted(),
        1,
        "an unsubscribed browser is not pushed again"
    );
}
