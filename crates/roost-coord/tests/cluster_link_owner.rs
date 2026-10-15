//! Cluster link ownership, replica-local probes and follower transport splicing.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::ws::{WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use futures::{SinkExt, StreamExt};
use roost_coord::cluster::ownership::{self, OwnershipState};
use roost_coord::services::CoordServices;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::Message as ClientMessage;

fn scratch() -> std::path::PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root =
        std::env::temp_dir().join(format!("roost-link-owner-{}-{stamp}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

async fn wait_until(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(4), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
}

async fn probe_status(address: SocketAddr) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = TcpStream::connect(address).await.unwrap();
    stream
        .write_all(b"GET /readyz HTTP/1.1\r\nHost: probe\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let status = std::str::from_utf8(&response)
        .unwrap()
        .lines()
        .next()
        .unwrap();
    status.split_whitespace().nth(1).unwrap().parse().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn postgres_replicas_elect_one_owner_release_and_detect_fencing() {
    if !db_support::running_on_postgres() {
        return;
    }
    let root = scratch();
    let db_a = db_support::open_test_database(&root).await.unwrap();
    let db_b = db_support::open_test_database(&root).await.unwrap();
    let service_a = Arc::new(CoordServices::new(db_a.clone()));
    let service_b = Arc::new(CoordServices::new(db_b.clone()));
    let state_a = Arc::clone(&service_a.ownership);
    let state_b = Arc::clone(&service_b.ownership);
    tokio::spawn(Arc::clone(&service_a.cluster).listen());
    tokio::spawn(Arc::clone(&service_b.cluster).listen());
    let (stop_a, rx_a) = watch::channel(false);
    let (_stop_b, rx_b) = watch::channel(false);
    let mut owner_a = Some(tokio::spawn(ownership::run(
        db_a,
        service_a.instance_id.clone(),
        "http://127.0.0.1:4113".into(),
        Arc::clone(&service_a.cluster),
        Arc::clone(&state_a),
        Arc::clone(&service_a.agent),
        rx_a,
    )));
    wait_until(|| state_a.is_owner()).await;

    let probe_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let probe_address = probe_listener.local_addr().unwrap();
    let probe_router = roost_coord::http::health::probe_routes().with_state(Arc::new(
        roost_coord::http::health::ProbeState {
            services: Arc::clone(&service_b),
        },
    ));
    tokio::spawn(async move {
        axum::serve(probe_listener, probe_router).await.unwrap();
    });
    assert_eq!(probe_status(probe_address).await, 503);

    let mut owner_b = Some(tokio::spawn(ownership::run(
        db_b.clone(),
        service_b.instance_id.clone(),
        "http://127.0.0.1:4113".into(),
        Arc::clone(&service_b.cluster),
        Arc::clone(&state_b),
        Arc::clone(&service_b.agent),
        rx_b,
    )));
    assert!(!state_b.is_owner());
    // Ready means all three probe inputs: a known owner, a fresh heartbeat
    // and a connected notice listener; on a slow runner they land apart.
    wait_until(|| {
        state_b.owner_internal_url().is_some()
            && state_b.heartbeat_fresh(roost_coord::serve::now_ms())
            && service_b.cluster.connected()
    })
    .await;
    assert_eq!(probe_status(probe_address).await, 200);

    stop_a.send(true).unwrap();
    owner_a.take().unwrap().await.unwrap().unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !state_b.is_owner() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    sqlx::query("UPDATE coord_leases SET owner_instance=$1,epoch=epoch+1,expires_ms=$2 WHERE name='cluster:links'")
        .bind("takeover")
        .bind(roost_coord::serve::now_ms().saturating_add(30_000))
        .execute(db_b.pool()).await.unwrap();
    let mut owner_b = owner_b.take().unwrap();
    let result = tokio::time::timeout(Duration::from_secs(4), &mut owner_b)
        .await
        .unwrap()
        .unwrap();
    assert!(result.is_err());
    owner_b.abort();
    std::fs::remove_dir_all(root).unwrap();
}

async fn owner_websocket(socket: WebSocket) {
    let (mut sender, mut receiver) = socket.split();
    while let Some(Ok(message)) = receiver.next().await {
        if sender.send(message).await.is_err() {
            break;
        }
    }
}

async fn ws_route(upgrade: WebSocketUpgrade) -> impl IntoResponse {
    upgrade.on_upgrade(owner_websocket)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn follower_splice_preserves_http_and_websocket_traffic() {
    let owner_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let owner_address = owner_listener.local_addr().unwrap();
    let owner_app = Router::new()
        .route("/", get(|| async { "owner response" }))
        .route("/ws", get(ws_route));
    tokio::spawn(async move {
        axum::serve(owner_listener, owner_app).await.unwrap();
    });
    let follower_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let follower_address = follower_listener.local_addr().unwrap();
    let state = Arc::new(OwnershipState::default());
    state._set_owner_url_for_tests(Some(format!("http://{owner_address}")));
    tokio::spawn(roost_coord::cluster::splice::serve(
        follower_listener,
        Arc::clone(&state),
    ));

    let mut http = TcpStream::connect(follower_address).await.unwrap();
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    http.write_all(
        format!("GET / HTTP/1.1\r\nHost: {follower_address}\r\nConnection: close\r\n\r\n")
            .as_bytes(),
    )
    .await
    .unwrap();
    let mut response = Vec::new();
    http.read_to_end(&mut response).await.unwrap();
    assert!(response.ends_with(b"owner response"));

    let (mut websocket, _) =
        tokio_tungstenite::connect_async(format!("ws://{follower_address}/ws"))
            .await
            .unwrap();
    websocket
        .send(ClientMessage::Text("through follower".into()))
        .await
        .unwrap();
    let echoed = websocket.next().await.unwrap().unwrap();
    assert_eq!(echoed.into_text().unwrap(), "through follower");
}
