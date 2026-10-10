use std::{collections::BTreeMap, sync::Arc};

use axum::{Json, Router, extract::State, response::Response, routing::post};
use futures::StreamExt;
use reqwest::StatusCode;
use roost_llm::{
    ChatRequest, Endpoints, Message, ResolvedAuth, StreamEvent, ToolSpec, stream_chat,
};
use serde_json::Value;
use tokio::{net::TcpListener, sync::Mutex};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct ServerState {
    body: &'static str,
    status: StatusCode,
    requests: Arc<Mutex<Vec<Value>>>,
    headers: Arc<Mutex<Vec<axum::http::HeaderMap>>>,
}

async fn serve_request(
    State(state): State<ServerState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<Value>,
) -> Response<String> {
    state.requests.lock().await.push(body);
    state.headers.lock().await.push(headers);
    let mut response = Response::builder()
        .status(state.status)
        .header("content-type", "text/event-stream");
    if state.status == StatusCode::TOO_MANY_REQUESTS {
        response = response
            .header("retry-after", "2")
            .header("anthropic-ratelimit-unified-5h-reset", "0:00:05.500");
    }
    response.body(state.body.to_owned()).unwrap()
}

pub async fn server(
    body: &'static str,
    status: StatusCode,
) -> (
    String,
    Arc<Mutex<Vec<Value>>>,
    Arc<Mutex<Vec<axum::http::HeaderMap>>>,
) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let headers = Arc::new(Mutex::new(Vec::new()));
    let state = ServerState {
        body,
        status,
        requests: requests.clone(),
        headers: headers.clone(),
    };
    let app = Router::new()
        .route("/{*path}", post(serve_request))
        .with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}"), requests, headers)
}

pub fn request(model: &roost_llm::ModelInfo, tools: Vec<ToolSpec>) -> ChatRequest {
    ChatRequest {
        model: model.clone(),
        system: vec!["System instructions".into()],
        messages: vec![Message::user_text("inspect the project")],
        tools,
        thinking: "high".into(),
        session_id: "session-test".into(),
        max_tokens: Some(4096),
    }
}

pub fn auth() -> ResolvedAuth {
    ResolvedAuth::ApiKey {
        key: "test-key".into(),
    }
}

pub async fn events(request: ChatRequest, endpoint: &str) -> Vec<StreamEvent> {
    let mut overrides = BTreeMap::new();
    overrides.insert(request.model.provider.clone(), endpoint.to_owned());
    stream_chat(
        &reqwest::Client::new(),
        &Endpoints::with_overrides(overrides),
        request,
        auth(),
        CancellationToken::new(),
        None,
    )
    .collect::<Vec<_>>()
    .await
    .into_iter()
    .map(Result::unwrap)
    .collect()
}
