//! Two coordinator replicas over one database for the agent cluster tests:
//! each has its own pool, runtime and loopback fake Anthropic server, so a
//! test can tell which replica asked the model, and a tool executor that
//! blocks on demand so a run can be caught mid-tool.

#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use futures::future::BoxFuture;
use parking_lot::Mutex;
use roost_agent::{RuntimeConfig, ToolCall, ToolExecutor, ToolOutcome};
use roost_coord::agent::{AgentService, ClusterMembership};
use roost_coord::cluster::notify::ClusterNotifier;
use roost_coord::db::CoordDb;
use roost_coord::events::bus_domains::Buses;
use roost_llm::CredentialKind;
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

pub const MODEL: &str = "claude-sonnet-5";

/// A fake Messages endpoint. Replies come from a queue shared by both
/// replicas; each server keeps its own request log.
#[derive(Clone, Default)]
pub struct FakeAnthropic {
    pub replies: Arc<Mutex<Vec<String>>>,
    pub bodies: Arc<Mutex<Vec<Value>>>,
}

async fn messages(State(fake): State<FakeAnthropic>, Json(body): Json<Value>) -> Response {
    fake.bodies.lock().push(body);
    let reply = {
        let mut replies = fake.replies.lock();
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

pub fn text_sse(text: &str) -> String {
    sse(&[
        serde_json::json!({"type":"message_start","message":{"usage":{"input_tokens":10,"output_tokens":0}}}),
        serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}),
        serde_json::json!({"type":"content_block_stop","index":0}),
        serde_json::json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}),
        serde_json::json!({"type":"message_stop"}),
    ])
}

pub fn tool_sse(call_id: &str, name: &str, args: &str) -> String {
    sse(&[
        serde_json::json!({"type":"message_start","message":{"usage":{"input_tokens":10,"output_tokens":0}}}),
        serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":call_id,"name":name,"input":{}}}),
        serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":args}}),
        serde_json::json!({"type":"content_block_stop","index":0}),
        serde_json::json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":5}}),
        serde_json::json!({"type":"message_stop"}),
    ])
}

/// Serves `fake` on a loopback port and returns its base URL.
pub async fn serve_fake(fake: FakeAnthropic) -> String {
    let app = Router::new()
        .route("/v1/messages", post(messages))
        .with_state(fake);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{address}")
}

/// The worker. `bash` blocks until `release` (or cancellation) when `hold`
/// is set; every other tool answers at once.
#[derive(Default)]
pub struct FakeWorker {
    pub calls: Mutex<Vec<ToolCall>>,
    pub hold: Mutex<bool>,
    pub started: Notify,
    pub release: Notify,
}

impl ToolExecutor for FakeWorker {
    fn execute<'a>(
        &'a self,
        _worker_fp: &'a str,
        call: ToolCall,
        _out: mpsc::Sender<String>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<ToolOutcome, String>> {
        let blocks = call.tool == "bash" && *self.hold.lock();
        let content = if call.tool == "context_files" {
            r#"{"context":"","watchdog":""}"#.to_owned()
        } else {
            "done".to_owned()
        };
        self.calls.lock().push(call);
        Box::pin(async move {
            if blocks {
                self.started.notify_one();
                tokio::select! {
                    () = self.release.notified() => {}
                    () = cancel.cancelled() => return Err("cancelled".into()),
                }
            }
            Ok(ToolOutcome {
                is_error: false,
                content,
                details_json: "{}".into(),
            })
        })
    }

    fn close_conversation<'a>(&'a self, _: &'a str, _: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

impl FakeWorker {
    pub fn bash_calls(&self) -> usize {
        self.calls
            .lock()
            .iter()
            .filter(|call| call.tool == "bash")
            .count()
    }
}

/// One replica: its own pool, notifier, model endpoint and worker.
pub struct Replica {
    pub service: Arc<AgentService>,
    pub fake: FakeAnthropic,
    pub worker: Arc<FakeWorker>,
    pub db: CoordDb,
    pub buses: Arc<Buses>,
}

/// Opens a replica over the database `root` names. On SQLite the replicas
/// share `notifier`, standing in for `pg_notify`; on Postgres each listens.
pub async fn replica(
    root: &Path,
    name: &str,
    replies: Arc<Mutex<Vec<String>>>,
    shared_notifier: Option<Arc<ClusterNotifier>>,
) -> Replica {
    let db = super::db_support::open_test_database(root)
        .await
        .expect("a migrated database");
    let notifier = if super::db_support::running_on_postgres() {
        ClusterNotifier::new(db.clone())
    } else {
        shared_notifier.unwrap_or_else(|| ClusterNotifier::new(db.clone()))
    };
    let fake = FakeAnthropic {
        replies,
        bodies: Arc::default(),
    };
    let endpoint = serve_fake(fake.clone()).await;
    let worker = Arc::new(FakeWorker::default());
    let buses = Buses::shared();
    let service = AgentService::with_config(
        db.clone(),
        Arc::clone(&buses),
        Arc::clone(&worker) as Arc<dyn ToolExecutor>,
        BTreeMap::from([("anthropic".to_owned(), endpoint)]),
        ClusterMembership {
            instance_id: name.to_owned(),
            notifier,
        },
        RuntimeConfig {
            retry_backoff: vec![Duration::from_millis(5); 3],
            advisor_backoff: vec![Duration::from_millis(5); 3],
            instance_id: name.to_owned(),
            lease_ttl: Duration::from_millis(1_500),
            lease_renew: Duration::from_millis(300),
            ..RuntimeConfig::default()
        },
        None,
    );
    tokio::spawn(Arc::clone(&service.cluster.notifier).listen());
    service.join_cluster();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !service.cluster.notifier.connected() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the notice listener connects");
    Replica {
        service,
        fake,
        worker,
        db,
        buses,
    }
}

pub async fn add_key(replica: &Replica) {
    replica
        .service
        .pool()
        .store()
        .upsert(
            "anthropic",
            CredentialKind::ApiKey {
                key: "sk-ant-cluster-key-0001".into(),
            },
            "cluster-key",
            "cluster key",
        )
        .await
        .expect("the fixture credential is stored");
}

/// Waits until `id` has no running state on `replica`'s view of the store.
pub async fn settled(replica: &Replica, id: &str, within: Duration) {
    tokio::time::timeout(within, async {
        loop {
            let transcript = replica.service.runtime.transcript(id).await.unwrap();
            if transcript.run_state != roost_protocol::wire::agent_chat::AgentRunState::Running {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the run settles");
}

/// User-message texts sent in one model request.
pub fn user_texts(body: &Value) -> Vec<String> {
    body["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "user")
        .flat_map(|message| match &message["content"] {
            Value::String(text) => vec![text.clone()],
            Value::Array(blocks) => blocks
                .iter()
                .filter_map(|block| block["text"].as_str().map(str::to_owned))
                .collect(),
            _ => Vec::new(),
        })
        .collect()
}
