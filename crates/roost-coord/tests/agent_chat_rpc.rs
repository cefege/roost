//! Agent chat RPCs over the coordinator's Rust harness, against a loopback
//! fake Anthropic Messages server and a tool executor that answers for the
//! worker: a full tool round reaches the Sync bus, a rate-limited account
//! rotates to the next, settings are validated, and restarts settle runs.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod agent_fixture;
mod db_support;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_fixture::{AgentFixture, WORKER_A};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use connectrpc::ErrorCode;
use futures::future::BoxFuture;
use roost_agent::{AgentStore, Entry, ToolCall, ToolExecutor, ToolOutcome};
use roost_coord::agent::AgentService;
use roost_coord::agent::rpc_accounts::handle_agent_settings_set;
use roost_coord::agent::rpc_auth::handle_agent_auth_set_api_key;
use roost_coord::agent::rpc_chat::{
    handle_agent_chat_create, handle_agent_chat_snapshot, handle_agent_chat_submit,
};
use roost_proto as proto;
use roost_protocol::wire::agent_chat::AgentRunState;
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const MODEL: &str = "claude-sonnet-5";

/// Answers each `messages` call from a queue; a key on the rate-limited list
/// gets HTTP 429 instead.
#[derive(Clone, Default)]
struct FakeAnthropic {
    replies: Arc<Mutex<Vec<String>>>,
    limited_keys: Arc<Mutex<Vec<String>>>,
    seen_keys: Arc<Mutex<Vec<String>>>,
}

async fn messages(
    State(fake): State<FakeAnthropic>,
    headers: HeaderMap,
    Json(_body): Json<Value>,
) -> Response {
    let key = headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    fake.seen_keys.lock().unwrap().push(key.clone());
    if fake.limited_keys.lock().unwrap().contains(&key) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", "30")],
            "slow down",
        )
            .into_response();
    }
    let reply = {
        let mut replies = fake.replies.lock().unwrap();
        if replies.is_empty() {
            text_sse("out of script")
        } else {
            replies.remove(0)
        }
    };
    ([("content-type", "text/event-stream")], reply).into_response()
}

fn sse(events: &[Value]) -> String {
    events
        .iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect()
}

fn text_sse(text: &str) -> String {
    sse(&[
        serde_json::json!({"type":"message_start","message":{"usage":{"input_tokens":10,"output_tokens":0}}}),
        serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}),
        serde_json::json!({"type":"content_block_stop","index":0}),
        serde_json::json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}),
        serde_json::json!({"type":"message_stop"}),
    ])
}

fn tool_sse(call_id: &str, name: &str, args: &str) -> String {
    sse(&[
        serde_json::json!({"type":"message_start","message":{"usage":{"input_tokens":10,"output_tokens":0}}}),
        serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":call_id,"name":name,"input":{}}}),
        serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":args}}),
        serde_json::json!({"type":"content_block_stop","index":0}),
        serde_json::json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":5}}),
        serde_json::json!({"type":"message_stop"}),
    ])
}

/// Stands in for the worker: `read` answers with a hashline file.
#[derive(Debug, Default)]
struct FakeWorker {
    calls: Mutex<Vec<ToolCall>>,
}

impl ToolExecutor for FakeWorker {
    fn execute<'a>(
        &'a self,
        _worker_fp: &'a str,
        call: ToolCall,
        _out: mpsc::Sender<String>,
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<ToolOutcome, String>> {
        let content = if call.tool == "context_files" {
            r#"{"context":"","watchdog":""}"#.to_owned()
        } else {
            "[README.md#5BF9]\n1:hello".to_owned()
        };
        self.calls.lock().unwrap().push(call);
        Box::pin(async move {
            Ok(ToolOutcome {
                is_error: false,
                content,
                details_json: "{}".into(),
            })
        })
    }

    fn close_conversation<'a>(
        &'a self,
        _worker_fp: &'a str,
        _conversation_id: &'a str,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

struct Setup {
    fixture: AgentFixture,
    fake: FakeAnthropic,
    worker: Arc<FakeWorker>,
}

async fn setup(label: &str) -> Setup {
    let fake = FakeAnthropic::default();
    let app = Router::new()
        .route("/v1/messages", post(messages))
        .with_state(fake.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let mut fixture = AgentFixture::new(label).await;
    let worker = Arc::new(FakeWorker::default());
    let services = Arc::get_mut(&mut fixture.core.services).expect("fixture owns services");
    services.agent = AgentService::with_tools(
        services.db.clone(),
        Arc::clone(&services.buses),
        Arc::clone(&worker) as Arc<dyn ToolExecutor>,
        BTreeMap::from([("anthropic".to_owned(), format!("http://{address}"))]),
    );
    Setup {
        fixture,
        fake,
        worker,
    }
}

async fn add_key(setup: &Setup, key: &str) {
    handle_agent_auth_set_api_key(
        &setup.fixture.core,
        &setup.fixture.caller,
        proto::AgentAuthSetApiKeyRequest {
            provider: "anthropic".into(),
            api_key: key.into(),
            ..Default::default()
        },
    )
    .await
    .expect("API key stored");
}

async fn create(setup: &Setup) -> String {
    handle_agent_chat_create(
        &setup.fixture.core,
        &setup.fixture.caller,
        proto::AgentChatCreateRequest {
            worker_fp: WORKER_A.into(),
            cwd: "/repo".into(),
            model_provider: "anthropic".into(),
            model_id: MODEL.into(),
            ..Default::default()
        },
    )
    .await
    .expect("conversation created")
    .body
    .id
}

async fn submit(setup: &Setup, id: &str, text: &str) {
    handle_agent_chat_submit(
        &setup.fixture.core,
        &setup.fixture.caller,
        proto::AgentChatSubmitRequest {
            conversation_id: id.into(),
            text: text.into(),
            ..Default::default()
        },
    )
    .await
    .expect("submitted");
}

async fn settled_transcript(setup: &Setup, id: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let snapshot = handle_agent_chat_snapshot(
                &setup.fixture.core,
                &setup.fixture.caller,
                proto::AgentChatSnapshotRequest {
                    conversation_id: id.into(),
                    ..Default::default()
                },
            )
            .await
            .expect("snapshot")
            .body;
            let transcript: Value = serde_json::from_str(&snapshot.transcript_json).unwrap();
            if transcript["run_state"] != "running" {
                return transcript;
            }
        }
    })
    .await
    .expect("run settles")
}

#[tokio::test]
async fn a_submitted_message_runs_a_tool_round_and_publishes_the_transcript() {
    let setup = setup("agent-chat-tool-round").await;
    add_key(&setup, "sk-ant-first-key-0001").await;
    setup.fake.replies.lock().unwrap().extend([
        tool_sse("call-1", "read", r#"{"path":"README.md"}"#),
        text_sse("The README says hello."),
    ]);
    let (published, mut published_rx) = mpsc::unbounded_channel();
    let _subscription = setup
        .fixture
        .core
        .services
        .buses
        .agent_chat_bus
        .subscribe(move |update| {
            let _ = published.send(update.clone());
        });
    let id = create(&setup).await;
    submit(&setup, &id, "what does the README say?").await;
    let transcript = settled_transcript(&setup, &id).await;

    assert_eq!(transcript["run_state"], "idle", "{transcript}");
    let items = transcript["items"].as_array().unwrap();
    assert!(
        items
            .iter()
            .any(|item| item["kind"] == "tool"
                && item["output"].as_str().unwrap().contains("1:hello"))
    );
    assert!(items.iter().any(|item| {
        item["kind"] == "assistant"
            && item["blocks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|block| block["text"] == "The README says hello.")
    }));
    let reads: Vec<ToolCall> = setup
        .worker
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|call| call.tool == "read")
        .cloned()
        .collect();
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].cwd, "/repo");

    let mut bus_text = String::new();
    while let Ok(update) = published_rx.try_recv() {
        if update.conversation_id == id {
            bus_text.push_str(&update.events_json);
        }
    }
    assert!(
        bus_text.contains("The README says hello."),
        "the Sync bus carried the answer"
    );
}

#[tokio::test]
async fn a_rate_limited_account_rotates_to_the_next_one() {
    let setup = setup("agent-chat-rotation").await;
    add_key(&setup, "sk-ant-first-key-0001").await;
    add_key(&setup, "sk-ant-second-key-0002").await;
    setup
        .fake
        .limited_keys
        .lock()
        .unwrap()
        .push("sk-ant-first-key-0001".into());
    setup.fake.replies.lock().unwrap().push(text_sse("served"));
    let id = create(&setup).await;
    // Whichever account the pool tries first, the run must end on the second.
    submit(&setup, &id, "hi").await;
    let transcript = settled_transcript(&setup, &id).await;
    assert_eq!(transcript["run_state"], "idle", "{transcript}");
    let answered = transcript["items"].as_array().unwrap().iter().any(|item| {
        item["kind"] == "assistant"
            && item["blocks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|block| block["text"] == "served")
    });
    assert!(
        answered,
        "the run answered through the unthrottled account: {transcript}"
    );
    // Judgments after the answer resolve accounts on their own, so only the
    // presence of the second key is a fact about rotation, not its position.
    assert!(
        setup
            .fake
            .seen_keys
            .lock()
            .unwrap()
            .iter()
            .any(|key| key == "sk-ant-second-key-0002")
    );
}

#[tokio::test]
async fn an_invalid_role_selector_is_rejected() {
    let setup = setup("agent-settings-invalid").await;
    let error = handle_agent_settings_set(
        &setup.fixture.core,
        &setup.fixture.caller,
        proto::AgentSettingsSetRequest {
            settings_json: r#"{"model_roles":{"smol":"not a selector"}}"#.into(),
            ..Default::default()
        },
    )
    .await
    .expect_err("an invalid selector was accepted");
    assert_eq!(error.code, ErrorCode::InvalidArgument);
}

#[tokio::test]
async fn a_run_interrupted_by_a_restart_comes_back_idle_with_a_notice() {
    let setup = setup("agent-restart-recovery").await;
    let id = create(&setup).await;
    let store =
        roost_coord::agent::store::CoordAgentStore::new(setup.fixture.core.services.db.clone());
    let mut record = store.conversation(&id).await.unwrap().unwrap();
    record.run_state = AgentRunState::Running;
    store.save_conversation(&record).await.unwrap();

    setup.fixture.core.services.agent.recover().await;

    let record = store.conversation(&id).await.unwrap().unwrap();
    assert_eq!(record.run_state, AgentRunState::Idle);
    let entries = store.entries(&id).await.unwrap();
    assert!(entries.iter().any(|(_, entry)| matches!(entry,
        Entry::Notice { body, .. } if body == "Run interrupted by a coordinator restart")));
}
