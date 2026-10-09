//! Agent chat RPCs against a fake agent host, including the NDJSON follower's
//! transcript cache and install-wide event publication.
//!
//! The host listener is real loopback HTTP: these tests pin the coordinator's
//! authentication, HTTP contract, cache projection and Connect error boundary.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod agent_fixture;
mod db_support;

use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_fixture::AgentFixture;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use connectrpc::ErrorCode;
use futures_util::stream;
use roost_coord::agent_host::rpc_chat::{
    handle_agent_chat_list, handle_agent_chat_snapshot, handle_agent_chat_submit,
};
use roost_coord::agent_host::{FollowerHandle, spawn_follower};
use roost_host::{CoordConfig, CoordConfigInput, DatabaseLocation};
use roost_proto as proto;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

const CONVERSATION_ID: &str = "conversation-1";
const SECRET: &str = "agent-host-test-secret-with-at-least-32-bytes";

#[derive(Debug, Clone, PartialEq, Eq)]
struct RecordedRequest {
    method: Method,
    path: String,
    authorization: Option<String>,
    body: Vec<u8>,
}

#[derive(Clone)]
struct HostState {
    events: Vec<String>,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
}

struct FakeHost {
    base: String,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    task: tokio::task::JoinHandle<()>,
}

impl FakeHost {
    async fn start(events: Vec<Value>) -> Self {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let events = events
            .into_iter()
            .map(|event| format!("{}\n", event))
            .collect();
        let state = HostState {
            events,
            requests: Arc::clone(&requests),
        };
        let app = Router::new()
            .route("/v1/events", get(events_handler))
            .route("/v1/conversations/{id}/submit", post(submit_handler))
            .with_state(state);
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake host");
        let address = listener.local_addr().expect("fake host address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("fake host server");
        });
        Self {
            base: format!("http://{address}"),
            requests,
            task,
        }
    }

    fn recorded(&self) -> Vec<RecordedRequest> {
        self.requests.lock().expect("request recorder lock").clone()
    }

    async fn stop(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

async fn events_handler(State(state): State<HostState>, headers: HeaderMap) -> Response {
    state.record(Method::GET, "/v1/events", headers, Vec::new());
    let chunks = state
        .events
        .into_iter()
        .map(|line| Ok::<Bytes, Infallible>(Bytes::from(line)));
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/x-ndjson")
        .body(Body::from_stream(stream::iter(chunks)))
        .expect("event stream response")
}

async fn submit_handler(
    State(state): State<HostState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = format!("/v1/conversations/{id}/submit");
    state.record(Method::POST, &path, headers, body.to_vec());
    if id == "missing" {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": {"code": "not_found", "message": "conversation missing"}})),
        )
            .into_response();
    }
    (StatusCode::OK, Json(json!({"ok": true}))).into_response()
}

impl HostState {
    fn record(&self, method: Method, path: &str, headers: HeaderMap, body: Vec<u8>) {
        self.requests
            .lock()
            .expect("request recorder lock")
            .push(RecordedRequest {
                method,
                path: path.to_owned(),
                authorization: headers
                    .get(axum::http::header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned),
                body,
            });
    }
}

fn conversation() -> Value {
    json!({
        "id": CONVERSATION_ID,
        "title": "Test conversation",
        "worker_fp": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "worker_label": "worker-a",
        "cwd": "/work",
        "model": null,
        "thinking_level": null,
        "run_state": "idle",
        "error": null,
        "created_ms": 1,
        "updated_ms": 2
    })
}

fn stream_events() -> Vec<Value> {
    vec![
        json!({"type": "conversations", "conversations": [conversation()]}),
        json!({
            "type": "chat",
            "conversation_id": CONVERSATION_ID,
            "events": [{
                "type": "reset",
                "transcript": {
                    "items": [{"kind": "assistant", "id": "assistant-1", "blocks": [], "streaming": true, "error": null}],
                    "run_state": "running",
                    "error": null,
                    "model": null,
                    "thinking_level": null,
                    "usage": {"input_tokens": 0, "output_tokens": 0, "cost_usd": 0.0}
                }
            }]
        }),
        json!({
            "type": "chat",
            "conversation_id": CONVERSATION_ID,
            "events": [{"type": "text_delta", "item_id": "assistant-1", "block": 0, "delta": "hello from host"}]
        }),
    ]
}

async fn configured_fixture(host: &FakeHost, label: &str) -> AgentFixture {
    let mut fixture = AgentFixture::new(label).await;
    let config = CoordConfig::parse(CoordConfigInput {
        database: Some(DatabaseLocation::SqliteFile(
            std::env::temp_dir().join(format!("{label}.db")),
        )),
        authorized_keys_path: Some(std::env::temp_dir().join(format!("{label}-keys"))),
        log_dir: Some(std::env::temp_dir().join(format!("{label}-logs"))),
        agent_host_url: Some(host.base.clone()),
        agent_host_secret: Some(SECRET.to_owned()),
        ..Default::default()
    })
    .expect("agent host coordinator config");
    let services = Arc::get_mut(&mut fixture.core.services).expect("fixture owns services");
    services.boot.config = Some(Arc::new(config));
    fixture
}

async fn wait_for_disconnected_list(fixture: &AgentFixture) -> proto::AgentChatListResponse {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let list = handle_agent_chat_list(
                &fixture.core,
                &fixture.caller,
                proto::AgentChatListRequest::default(),
            )
            .await
            .expect("cached conversation list")
            .body;
            if !list.host_connected
                && list
                    .conversations
                    .iter()
                    .any(|item| item.id == CONVERSATION_ID)
            {
                return list;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("closed event stream marks host disconnected")
}

async fn stop_follower(follower: FollowerHandle) {
    follower.stop().await;
}

#[tokio::test]
async fn follower_stream_projects_snapshot_and_publishes_ordered_chat_events() {
    let host = FakeHost::start(stream_events()).await;
    let fixture = configured_fixture(&host, "agent-chat-rpc-stream").await;
    let (published_tx, mut published_rx) = mpsc::unbounded_channel();
    let _subscription = fixture
        .core
        .services
        .buses
        .agent_chat_bus
        .subscribe(move |update| {
            let _ = published_tx.send(update.clone());
        });
    let follower = spawn_follower(Arc::clone(&fixture.core.services));

    let mut seq_two = None;
    tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(update) = published_rx.recv().await {
            if update.conversation_id == CONVERSATION_ID && update.seq == 2 {
                seq_two = Some(update);
                break;
            }
        }
    })
    .await
    .expect("second chat event is published");

    let snapshot = handle_agent_chat_snapshot(
        &fixture.core,
        &fixture.caller,
        proto::AgentChatSnapshotRequest {
            conversation_id: CONVERSATION_ID.to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect("cached transcript snapshot")
    .body;
    assert_eq!(snapshot.seq, 2);
    let transcript: Value = serde_json::from_str(&snapshot.transcript_json).expect("snapshot JSON");
    assert_eq!(
        transcript["items"][0]["blocks"][0]["text"],
        "hello from host"
    );
    let published = seq_two.expect("publication at sequence two");
    assert_eq!(
        published.events_json,
        json!([{"type":"text_delta","item_id":"assistant-1","block":0,"delta":"hello from host"}])
            .to_string()
    );

    let list = wait_for_disconnected_list(&fixture).await;
    assert_eq!(list.conversations.len(), 1);
    assert_eq!(list.conversations[0].id, CONVERSATION_ID);
    assert!(
        !list.host_connected,
        "the finite fake stream has been closed by its host"
    );

    stop_follower(follower).await;
    host.stop().await;
}

#[tokio::test]
async fn submit_forwards_bearer_and_body_and_maps_host_not_found() {
    let host = FakeHost::start(Vec::new()).await;
    let fixture = configured_fixture(&host, "agent-chat-rpc-submit").await;
    let request = proto::AgentChatSubmitRequest {
        conversation_id: "conversation-1".to_owned(),
        text: "please continue".to_owned(),
        request_id: "request-7".to_owned(),
        ..Default::default()
    };
    handle_agent_chat_submit(&fixture.core, &fixture.caller, request)
        .await
        .expect("submit reaches host");

    let error = handle_agent_chat_submit(
        &fixture.core,
        &fixture.caller,
        proto::AgentChatSubmitRequest {
            conversation_id: "missing".to_owned(),
            text: "ignored".to_owned(),
            request_id: "request-8".to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect_err("host not_found is returned to caller");
    assert_eq!(error.code, ErrorCode::NotFound);

    let recorded = host.recorded();
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0].method, Method::POST);
    assert_eq!(recorded[0].path, "/v1/conversations/conversation-1/submit");
    let expected_bearer = format!("Bearer {SECRET}");
    assert_eq!(
        recorded[0].authorization.as_deref(),
        Some(expected_bearer.as_str())
    );
    let body: Value = serde_json::from_slice(&recorded[0].body).expect("submit request JSON");
    assert_eq!(
        body,
        json!({"text":"please continue", "request_id":"request-7"})
    );
    assert_eq!(recorded[1].path, "/v1/conversations/missing/submit");

    host.stop().await;
}

#[tokio::test]
async fn unconfigured_agent_host_is_unavailable() {
    let mut fixture = AgentFixture::new("agent-chat-rpc-unconfigured").await;
    let config = CoordConfig::parse(CoordConfigInput {
        database: Some(DatabaseLocation::SqliteFile(
            std::env::temp_dir().join("agent-chat-rpc-unconfigured.db"),
        )),
        authorized_keys_path: Some(std::env::temp_dir().join("agent-chat-rpc-unconfigured-keys")),
        log_dir: Some(std::env::temp_dir().join("agent-chat-rpc-unconfigured-logs")),
        ..Default::default()
    })
    .expect("unconfigured coordinator config");
    Arc::get_mut(&mut fixture.core.services)
        .expect("fixture owns services")
        .boot
        .config = Some(Arc::new(config));
    let error = handle_agent_chat_list(
        &fixture.core,
        &fixture.caller,
        proto::AgentChatListRequest::default(),
    )
    .await
    .expect_err("agent host is not configured");
    assert_eq!(error.code, ErrorCode::Unavailable);
}
