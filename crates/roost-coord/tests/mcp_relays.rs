//! The MCP relay registry over a real migrated database: the four methods, the
//! stream each mutation publishes, the scope that keeps a call inside the
//! dashboard it was issued for, and the bound on a store that cannot answer.
//!
//! The surface is install-wide by construction -- no session column, no session
//! field on the proto, none on the stream frame -- so the boundary under test is
//! the dashboard the row belongs to. It is asserted with a row that exists and
//! is deliberately invisible, because the property is not "unknown ids are
//! refused" (v2 already had that) but "an id this coordinator does not hold
//! changes nothing here and reaches nobody's stream".

// `expect` and `unwrap` are denied outside `#[cfg(test)]`, and an integration
// test is its own crate rather than a module of one, so the exemption has to be
// stated here rather than inherited.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use connectrpc::{ConnectError, ErrorCode};
use roost_coord::auth::principal::Principal;
use roost_coord::auth::self_hosted_tenant::ensure_self_hosted_tenant;
use roost_coord::coord_core::{BootFacts, Caller, CoordCore, ListenerTrust};
use roost_coord::db::CoordDb;
use roost_coord::services::CoordServices;
use roost_coord::sessions::mcp::{
    handle_mcp_create, handle_mcp_delete, handle_mcp_list, handle_mcp_publish,
};
use roost_proto as proto;
use roost_protocol::wire::{McpRelayDelta, McpStreamMessage};
use sqlx::Row;

/// A dashboard this coordinator does not hold, so a row can be planted in it.
const FOREIGN_DASHBOARD: &str = "00000000-0000-4000-8000-00000000dead";

/// A relay planted in the foreign dashboard. Well formed, so only the scope can
/// refuse it.
const FOREIGN_RELAY: &str = "00000000-0000-4000-8000-00000000beef";

/// A relay id this coordinator does not hold, for the paths that need one and
/// do not care whether it exists.
const UNKNOWN_RELAY: &str = "00000000-0000-4000-8000-00000000cafe";

struct McpFixture {
    core: CoordCore,
    dashboard_id: String,
    root: PathBuf,
}

impl McpFixture {
    async fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-mcp-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let database = roost_coord::db::open(&root.join("coord.db"))
            .await
            .expect("a migrated coordinator database");
        let tenant = ensure_self_hosted_tenant(&database, 0)
            .await
            .expect("a self-hosted tenant");
        // The dashboard the handlers scope to is a boot fact, so the fixture
        // installs one: a coordinator built without it is what an unbooted
        // `CoordServices` is, and its refusal is asserted separately below.
        let services = CoordServices::booted(
            database,
            BootFacts {
                tenant: Some(tenant.clone()),
                ..BootFacts::unbooted()
            },
        );
        Self {
            core: CoordCore::new(Arc::new(services)),
            dashboard_id: tenant.dashboard_id,
            root,
        }
    }

    fn database(&self) -> &CoordDb {
        &self.core.services.db
    }

    fn device(&self) -> Caller {
        caller(Principal::AccountDevice {
            fingerprint: std::iter::repeat_n('a', 64).collect(),
            label: "MCP pane".to_owned(),
            account_id: "account-under-test".to_owned(),
        })
    }

    fn worker(&self) -> Caller {
        caller(Principal::Worker {
            fingerprint: "mcp-worker".to_owned(),
            label: "a worker".to_owned(),
        })
    }

    /// Run `body` with the relay stream collected, and hand back what it saw.
    async fn collect<T, F>(&self, body: F) -> (T, Vec<McpStreamMessage>)
    where
        F: Future<Output = T>,
    {
        let seen: Arc<Mutex<Vec<McpStreamMessage>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let subscription =
            self.core
                .services
                .buses
                .mcp_bus
                .subscribe(move |message: &McpStreamMessage| {
                    sink.lock()
                        .expect("the stream sink lock")
                        .push(message.clone());
                });
        let outcome = body.await;
        drop(subscription);
        let messages = seen.lock().expect("the stream sink lock").clone();
        (outcome, messages)
    }

    /// Every relay row, whatever dashboard holds it, as (id, dashboard_id).
    async fn rows(&self) -> Vec<(String, String)> {
        let rows = sqlx::query("SELECT id, dashboard_id FROM mcp_relays")
            .fetch_all(self.database().pool())
            .await
            .expect("the relay rows");
        rows.iter()
            .map(|row| {
                (
                    row.try_get::<String, _>("id").expect("the relay id"),
                    row.try_get::<String, _>("dashboard_id")
                        .expect("the relay dashboard"),
                )
            })
            .collect()
    }
}

impl Drop for McpFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn caller(principal: Principal) -> Caller {
    Caller {
        principal,
        tab_id: Some("tab-under-test".to_owned()),
        remote_address: Some("127.0.0.1".to_owned()),
        on_host: true,
        listener_trust: ListenerTrust::DirectLoopback,
    }
}

fn create_request(label: &str, kind: &str, config_json: &str) -> proto::McpCreateRequest {
    proto::McpCreateRequest {
        label: label.to_owned(),
        kind: kind.to_owned(),
        config_json: config_json.to_owned(),
        ..Default::default()
    }
}

fn publish_request(id: &str, payload_json: &str) -> proto::McpPublishRequest {
    proto::McpPublishRequest {
        id: id.to_owned(),
        payload_json: payload_json.to_owned(),
        ..Default::default()
    }
}

fn delete_request(id: &str) -> proto::McpDeleteRequest {
    proto::McpDeleteRequest {
        id: id.to_owned(),
        ..Default::default()
    }
}

fn message_of(error: &ConnectError) -> String {
    // `ConnectError::message` is a FIELD, an `Option<String>` carrying the text
    // the client will read, and `payload::message::<M>()` is a different thing
    // that decodes a body. A test asserting a refusal's wording wants the former.
    error.message.clone().unwrap_or_default()
}

/// Plant a relay that exists but belongs to a dashboard this coordinator does
/// not hold, which is the only way a scoped read can be proved.
///
/// The dashboard is a real row, because `mcp_relays.dashboard_id` is a foreign
/// key and the store runs with `foreign_keys(true)`: a relay pointing at no
/// dashboard would fail to insert, and the test would pass by never reaching
/// the scope it exists to exercise.
async fn plant_foreign_relay(fixture: &McpFixture) {
    sqlx::query(
        "INSERT INTO dashboards (id, organization_id, slug, name, status, created_at_ms) \
         SELECT ?1, organization_id, 'other-dashboard', 'Another dashboard', 'active', 0 \
         FROM dashboards LIMIT 1",
    )
    .bind(FOREIGN_DASHBOARD)
    .execute(fixture.database().pool())
    .await
    .expect("the foreign dashboard row");
    sqlx::query(
        "INSERT INTO mcp_relays (id, label, kind, config_json, created_at_ms, dashboard_id) \
         VALUES (?1, 'A relay of somebody else''s', 'sse', '{}', 5, ?2)",
    )
    .bind(FOREIGN_RELAY)
    .bind(FOREIGN_DASHBOARD)
    .execute(fixture.database().pool())
    .await
    .expect("the foreign relay row");
}

#[tokio::test]
async fn create_list_publish_and_delete_stay_consistent_with_the_relay_stream() {
    let fixture = McpFixture::new("stream").await;
    let device = fixture.device();
    let config = r#"{"command":"bun","args":["run","mcp"]}"#;

    let (created_id, messages) = fixture
        .collect(async {
            let created = handle_mcp_create(
                &fixture.core,
                &device,
                create_request("Local tools", "stdio", config),
            )
            .await
            .expect("a relay is registered")
            .body;
            let relay = created.relay.as_option().expect("the created relay");
            assert_eq!(relay.label, "Local tools");
            assert_eq!(relay.kind, "stdio");
            assert_eq!(
                relay.config_json, config,
                "the RPC echoes the caller's bytes rather than a re-rendering"
            );
            let id = relay.id.clone();

            let listed = handle_mcp_list(&fixture.core, &device, proto::McpListRequest::default())
                .await
                .expect("the registry is listed")
                .body;
            assert_eq!(listed.relays.len(), 1);
            assert_eq!(listed.relays[0].id, id);
            assert!(
                listed.relays[0].created_at_ms > 0,
                "the registry answers with the instant the row was written"
            );

            let published = handle_mcp_publish(
                &fixture.core,
                &device,
                publish_request(&id, r#"{"method":"tools/list"}"#),
            )
            .await
            .expect("a payload is accepted")
            .body;
            assert!(
                published.ok,
                "acceptance is what the method answers, and nothing more"
            );

            let deleted = handle_mcp_delete(&fixture.core, &device, delete_request(&id))
                .await
                .expect("the relay is removed")
                .body;
            assert!(deleted.ok);
            id
        })
        .await;

    assert!(
        handle_mcp_list(&fixture.core, &device, proto::McpListRequest::default())
            .await
            .expect("the registry is listed")
            .body
            .relays
            .is_empty(),
        "the deleted relay is gone from the registry"
    );

    assert_eq!(messages.len(), 3, "one stream message per state transition");
    let McpStreamMessage::Delta(McpRelayDelta::Created { relay }) = &messages[0] else {
        panic!("the first message announces the relay");
    };
    assert_eq!(relay.id.as_str(), created_id);
    assert_eq!(relay.label, "Local tools");
    assert_eq!(relay.kind.as_str(), "stdio");
    assert_eq!(
        relay.config.get("command").and_then(|value| value.as_str()),
        Some("bun"),
        "the stream carries the PARSED config, which is not the RPC's raw text"
    );
    let McpStreamMessage::Event(event) = &messages[1] else {
        panic!("the second message is the published payload");
    };
    assert_eq!(event.relay_id.as_str(), created_id);
    assert_eq!(
        event.payload.get("method").and_then(|value| value.as_str()),
        Some("tools/list")
    );
    assert!(event.ts > 0, "the payload is stamped when it was accepted");
    let McpStreamMessage::Delta(McpRelayDelta::Deleted { id }) = &messages[2] else {
        panic!("the third message removes the relay");
    };
    assert_eq!(id.as_str(), created_id);
}

#[tokio::test]
async fn a_created_relay_persists_the_resolved_dashboard_id() {
    let fixture = McpFixture::new("scope").await;
    let created = handle_mcp_create(
        &fixture.core,
        &fixture.device(),
        create_request(
            "Scoped tools",
            "sse",
            r#"{"url":"http://127.0.0.1:9999/sse"}"#,
        ),
    )
    .await
    .expect("a relay is registered")
    .body;
    let id = created
        .relay
        .as_option()
        .expect("the created relay")
        .id
        .clone();

    assert_eq!(
        fixture.rows().await,
        vec![(id, fixture.dashboard_id.clone())],
        "the row is written under the dashboard boot resolved, not under a literal"
    );
}

#[tokio::test]
async fn a_config_that_is_not_a_json_object_is_refused_before_any_row_persists() {
    let fixture = McpFixture::new("config").await;
    let (refused, messages) = fixture
        .collect(async {
            handle_mcp_create(
                &fixture.core,
                &fixture.device(),
                create_request("Broken", "stdio", "[1,2,3]"),
            )
            .await
            .expect_err("an array is not a relay config")
        })
        .await;

    assert_eq!(refused.code, ErrorCode::InvalidArgument);
    assert!(
        message_of(&refused).contains("configJson"),
        "the refusal names the field: {}",
        message_of(&refused)
    );
    assert!(
        fixture.rows().await.is_empty(),
        "a row the announcement cannot describe must not persist"
    );
    assert!(
        messages.is_empty(),
        "nothing was announced for a relay that does not exist"
    );
}

#[tokio::test]
async fn a_relay_another_dashboard_holds_is_not_found_and_changes_nothing() {
    let fixture = McpFixture::new("foreign").await;
    plant_foreign_relay(&fixture).await;
    let device = fixture.device();

    let (published, published_messages) = fixture
        .collect(async {
            handle_mcp_publish(
                &fixture.core,
                &device,
                publish_request(FOREIGN_RELAY, r#"{"method":"tools/list"}"#),
            )
            .await
        })
        .await;
    let published = published.expect_err("a relay outside this dashboard is not publishable");
    assert_eq!(published.code, ErrorCode::NotFound);
    assert!(
        published_messages.is_empty(),
        "a call for a relay this coordinator does not hold must reach no stream"
    );

    let deleted = handle_mcp_delete(&fixture.core, &device, delete_request(FOREIGN_RELAY))
        .await
        .expect_err("a relay outside this dashboard is not deletable");
    assert_eq!(
        deleted.code, published.code,
        "an unknown id and a foreign id are one answer, so the registry cannot be probed"
    );
    assert_eq!(
        fixture.rows().await,
        vec![(FOREIGN_RELAY.to_owned(), FOREIGN_DASHBOARD.to_owned())],
        "the refused delete left the row exactly as it was"
    );

    let listed = handle_mcp_list(&fixture.core, &device, proto::McpListRequest::default())
        .await
        .expect("the registry is listed")
        .body;
    assert!(
        listed.relays.is_empty(),
        "a registry answer names only what this dashboard holds"
    );
}

#[tokio::test]
async fn a_publish_the_store_cannot_answer_is_refused_inside_the_busy_timeout() {
    let fixture = McpFixture::new("deadline").await;
    // The coordinator keeps one connection, so holding it is what an unresponsive
    // store looks like from a handler: the statement can never start.
    let held = fixture
        .database()
        .pool()
        .acquire()
        .await
        .expect("the fixture takes the coordinator's only connection");

    let started = Instant::now();
    let refused = handle_mcp_publish(
        &fixture.core,
        &fixture.device(),
        publish_request(UNKNOWN_RELAY, r#"{"method":"tools/list"}"#),
    )
    .await
    .expect_err("a store that cannot answer is refused, not waited on");
    let elapsed = started.elapsed();

    assert_eq!(
        refused.code,
        ErrorCode::Unavailable,
        "the call is retryable, so it is neither the caller's fault nor a statement failure"
    );
    assert!(
        message_of(&refused).contains("McpPublish"),
        "the refusal names the method that was waiting: {}",
        message_of(&refused)
    );
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "the caller was held for {elapsed:?}, which is not a bound"
    );
    drop(held);
}

#[tokio::test]
async fn an_unrepresentable_payload_is_refused_before_the_relay_is_resolved() {
    let fixture = McpFixture::new("precedence").await;
    // The relay does not exist either, so the refusal identifies which of the
    // two was checked first: a payload the coordinator cannot represent is not
    // worth resolving a relay for.
    let refused = handle_mcp_publish(
        &fixture.core,
        &fixture.device(),
        publish_request(UNKNOWN_RELAY, "{not json"),
    )
    .await
    .expect_err("a payload that is not JSON is refused");
    assert_eq!(refused.code, ErrorCode::InvalidArgument);
    assert!(
        message_of(&refused).contains("payloadJson"),
        "the unparseable payload is named, not the missing relay: {}",
        message_of(&refused)
    );
}

#[tokio::test]
async fn a_malformed_relay_id_is_refused_before_it_reaches_the_store() {
    let fixture = McpFixture::new("ids").await;
    let deleted = handle_mcp_delete(
        &fixture.core,
        &fixture.device(),
        delete_request("not-a-relay"),
    )
    .await
    .expect_err("a relay id that is not a uuid is refused");
    assert_eq!(deleted.code, ErrorCode::InvalidArgument);
    assert!(
        message_of(&deleted).contains("relay id"),
        "the refusal names the field: {}",
        message_of(&deleted)
    );

    let published = handle_mcp_publish(&fixture.core, &fixture.device(), publish_request("", "{}"))
        .await
        .expect_err("an empty relay id is refused");
    assert_eq!(published.code, ErrorCode::InvalidArgument);
    assert!(fixture.rows().await.is_empty());
}

#[tokio::test]
async fn an_unknown_relay_kind_and_an_empty_label_are_refused() {
    let fixture = McpFixture::new("kind").await;
    let empty_label = handle_mcp_create(
        &fixture.core,
        &fixture.device(),
        create_request("", "stdio", "{}"),
    )
    .await
    .expect_err("a relay with no label is refused");
    assert_eq!(empty_label.code, ErrorCode::InvalidArgument);
    assert_eq!(message_of(&empty_label), "label is required");

    let bad_kind = handle_mcp_create(
        &fixture.core,
        &fixture.device(),
        create_request("Carrier pigeon", "smoke-signal", "{}"),
    )
    .await
    .expect_err("a kind the wire does not define is refused");
    assert_eq!(bad_kind.code, ErrorCode::InvalidArgument);
    assert!(
        message_of(&bad_kind).contains("smoke-signal"),
        "the refusal quotes what arrived: {}",
        message_of(&bad_kind)
    );
    assert!(fixture.rows().await.is_empty());
}

#[tokio::test]
async fn a_worker_principal_has_no_authority_over_the_relay_registry() {
    let fixture = McpFixture::new("worker").await;
    let worker = fixture.worker();

    let listed = handle_mcp_list(&fixture.core, &worker, proto::McpListRequest::default())
        .await
        .expect_err("a machine cannot read the registry");
    assert_eq!(listed.code, ErrorCode::Unauthenticated);

    let created = handle_mcp_create(
        &fixture.core,
        &worker,
        create_request("Worker tools", "stdio", "{}"),
    )
    .await
    .expect_err("a machine cannot register a relay");
    assert_eq!(created.code, ErrorCode::Unauthenticated);

    let published =
        handle_mcp_publish(&fixture.core, &worker, publish_request(UNKNOWN_RELAY, "{}"))
            .await
            .expect_err("a machine cannot publish a payload");
    assert_eq!(published.code, ErrorCode::Unauthenticated);

    let deleted = handle_mcp_delete(&fixture.core, &worker, delete_request(UNKNOWN_RELAY))
        .await
        .expect_err("a machine cannot remove a relay");
    assert_eq!(deleted.code, ErrorCode::Unauthenticated);
    assert!(fixture.rows().await.is_empty());
}

#[tokio::test]
async fn a_coordinator_with_no_tenancy_refuses_rather_than_reading_every_row() {
    let fixture = McpFixture::new("unbooted").await;
    // `CoordServices::new` is what a process built without the boot step gets,
    // and the answer has to name the wiring fault rather than fall back to "all
    // rows", which is a different registry rather than a refusal.
    let unbooted = CoordCore::new(Arc::new(CoordServices::new(fixture.database().clone())));
    let refused = handle_mcp_list(
        &unbooted,
        &fixture.device(),
        proto::McpListRequest::default(),
    )
    .await
    .expect_err("an unbooted coordinator has no registry to read");
    assert_eq!(refused.code, ErrorCode::Internal);
    assert_eq!(message_of(&refused), "coordinator booted without tenant");
}

#[tokio::test]
async fn the_registry_answers_in_the_order_it_declares() {
    let fixture = McpFixture::new("order").await;
    let device = fixture.device();
    // Two rows created inside the same millisecond, so the order cannot come
    // from the clock: it has to come from the statement. SQLite answers a bare
    // `SELECT` in rowid order, which is insertion order, and two ids minted in
    // sequence do not sort that way -- so this pins the `ORDER BY` instead of
    // restating whichever order happened to come out.
    let mut ids = Vec::new();
    for label in ["first", "second"] {
        let created =
            handle_mcp_create(&fixture.core, &device, create_request(label, "stdio", "{}"))
                .await
                .expect("a relay is registered")
                .body;
        ids.push(
            created
                .relay
                .as_option()
                .expect("the created relay")
                .id
                .clone(),
        );
    }

    let listed = handle_mcp_list(&fixture.core, &device, proto::McpListRequest::default())
        .await
        .expect("the registry is listed")
        .body;
    let answered: Vec<String> = listed.relays.iter().map(|relay| relay.id.clone()).collect();
    let mut by_id = ids.clone();
    by_id.sort();
    assert_eq!(
        answered, by_id,
        "with equal timestamps the declared order is the id order"
    );

    let asked_again = handle_mcp_list(&fixture.core, &device, proto::McpListRequest::default())
        .await
        .expect("the registry is listed")
        .body;
    assert_eq!(
        asked_again
            .relays
            .iter()
            .map(|relay| relay.id.clone())
            .collect::<Vec<_>>(),
        answered,
        "a second ask answers the same way"
    );
}
